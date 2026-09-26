//! Optional folder cache. The executable, decoded bytes, key, every option,
//! output naming context and analysis mode are part of the key. Output paths
//! and export identities are applied afresh by decompile_core after each hit.
use crate::decompile_core::{atomic_write_contained, atomic_write_contained_guarded, sha256_hex};
use luau_lifter::{DecompileArtifact, DecompileOptions};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

const ENTRY_LIMIT: u64 = 16 * 1024 * 1024;
const ENTRY_COUNT_LIMIT: usize = 20_000;
const MARKER: &[u8] = b"Tovek executable-keyed artifact cache v1\n";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Key {
    schema_version: u8,
    executable_sha256: String,
    bytecode_sha256: String,
    decode_key: u8,
    option_bits: u32,
    module_hint: Option<String>,
    analysis: bool,
    no_shared_tail: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    key: Key,
    artifact_sha256: String,
    artifact: DecompileArtifact,
}

struct Record {
    size: u64,
    touched: u64,
}

#[derive(Default)]
struct Inventory {
    records: BTreeMap<String, Record>,
    recency: BTreeSet<(u64, String)>,
    bytes: u64,
    clock: u64,
}

#[derive(Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    writes: AtomicU64,
    corrupt: AtomicU64,
    evictions: AtomicU64,
    bypasses: AtomicU64,
    io_errors: AtomicU64,
    raced_hits: AtomicU64,
}

pub(crate) struct Cache {
    root: PathBuf,
    _lock: File,
    executable_sha256: String,
    no_shared_tail: bool,
    max_bytes: u64,
    inventory: Mutex<Inventory>,
    // Only publication holds this lock. Never hold a lock across decompilation:
    // nested Rayon work may re-enter another folder task on the same worker.
    stripes: [Mutex<()>; 64],
    counters: Counters,
}

pub(crate) fn diagnostic_environment() -> bool {
    std::env::vars_os().any(|(name, _)| {
        let name = name.to_string_lossy().to_ascii_uppercase();
        (name.starts_with("MEDAL_") && name != "MEDAL_NO_SHARED_TAIL")
            || name == "DEINLINE_ANCHOR_TRACE"
    })
}

impl Cache {
    pub fn open(path: &Path, max_bytes: u64, input: &Path, output: &Path) -> Result<Self, String> {
        if max_bytes == 0 {
            return Err("cache byte limit must be positive".into());
        }
        fs::create_dir_all(path).map_err(|e| e.to_string())?;
        let root = fs::canonicalize(path).map_err(|e| e.to_string())?;
        for other in [input, output] {
            let other = fs::canonicalize(other).map_err(|e| e.to_string())?;
            if root.starts_with(&other) || other.starts_with(&root) {
                return Err("cache must be disjoint from input and output trees".into());
            }
        }
        let lock_path = root.join(".tovek-cache.lock");
        reject_link(&lock_path).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| e.to_string())?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .map_err(|e| format!("cache is in use or cannot be locked: {e}"))?;
        let marker = root.join(".tovek-cache-v1");
        reject_link(&marker).map_err(|e| e.to_string())?;
        let marker_bytes = || -> std::io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            File::open(&marker)?
                .take(MARKER.len() as u64 + 1)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        let initialized = match marker_bytes() {
            Ok(bytes) if bytes == MARKER => true,
            Ok(_) => return Err("cache marker has an unsupported format".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.to_string()),
        };
        let mut rows = Vec::new();
        for (index, item) in fs::read_dir(&root).map_err(|e| e.to_string())?.enumerate() {
            if index >= 100_000 {
                return Err("cache directory scan limit exceeded".into());
            }
            let item = item.map_err(|e| e.to_string())?;
            let name = item.file_name().to_string_lossy().into_owned();
            if !entry_name(&name) {
                continue;
            }
            let metadata = fs::symlink_metadata(item.path()).map_err(|e| e.to_string())?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("cache entry is not a regular file".into());
            }
            if rows.len() >= ENTRY_COUNT_LIMIT {
                return Err("cache entry count exceeds the supported limit".into());
            }
            rows.push((
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                name,
                metadata.len(),
            ));
        }
        if !initialized {
            if !rows.is_empty() {
                return Err("uninitialized cache contains preexisting entry files".into());
            }
            atomic_write_contained(&root, &marker, MARKER, false).map_err(|e| e.to_string())?;
        }
        rows.sort();
        let mut inventory = Inventory::default();
        for (_, name, size) in rows {
            inventory.clock += 1;
            inventory.bytes = inventory.bytes.saturating_add(size);
            inventory.recency.insert((inventory.clock, name.clone()));
            inventory.records.insert(
                name,
                Record {
                    size,
                    touched: inventory.clock,
                },
            );
        }
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let executable_sha256 = sha256_hex(&fs::read(executable).map_err(|e| e.to_string())?);
        let cache = Self {
            root,
            _lock: lock,
            executable_sha256,
            no_shared_tail: std::env::var_os("MEDAL_NO_SHARED_TAIL").is_some(),
            max_bytes,
            inventory: Mutex::new(inventory),
            stripes: std::array::from_fn(|_| Mutex::new(())),
            counters: Counters::default(),
        };
        // A lower quota takes effect even when every subsequent request hits.
        {
            let mut inventory = cache.inventory.lock();
            while inventory.bytes > cache.max_bytes {
                cache.evict_one(&mut inventory, None)?;
            }
        }
        Ok(cache)
    }

    fn key(
        &self,
        bytecode: &[u8],
        decode_key: u8,
        script: &str,
        options: DecompileOptions,
        analysis: bool,
    ) -> Key {
        Key {
            schema_version: 1,
            executable_sha256: self.executable_sha256.clone(),
            bytecode_sha256: sha256_hex(bytecode),
            decode_key,
            option_bits: options.bits(),
            module_hint: ast::name_locals::script_module_hint(script),
            analysis,
            no_shared_tail: self.no_shared_tail,
        }
    }

    pub fn get_or_compute(
        &self,
        bytecode: &[u8],
        decode_key: u8,
        script: &str,
        options: DecompileOptions,
        analysis: bool,
        compute: impl FnOnce() -> Result<DecompileArtifact, String>,
    ) -> Result<DecompileArtifact, String> {
        if bytecode.len() as u64 > ENTRY_LIMIT || script.len() > 4096 {
            self.counters.bypasses.fetch_add(1, Ordering::Relaxed);
            return compute();
        }
        let key = self.key(bytecode, decode_key, script, options, analysis);
        let digest = sha256_hex(&serde_json::to_vec(&key).expect("cache key serialization"));
        let name = format!("{digest}.json");
        let stripe = usize::from_str_radix(&digest[..2], 16).unwrap() % self.stripes.len();
        if let Some(artifact) = self.read(&name, &key) {
            self.counters.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(artifact);
        }
        self.counters.misses.fetch_add(1, Ordering::Relaxed);
        let artifact = compute()?; // Failures and panics are never cached.
        let _stripe = self.stripes[stripe].lock();
        if let Some(existing) = self.read(&name, &key) {
            if existing != artifact {
                return Err("same cache key produced inconsistent artifacts".into());
            }
            self.counters.raced_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(existing);
        }
        if let Err(error) = self.write(&name, key, &artifact) {
            self.counters.io_errors.fetch_add(1, Ordering::Relaxed);
            eprintln!("cache write skipped: {error}");
        }
        Ok(artifact)
    }

    fn read(&self, name: &str, key: &Key) -> Option<DecompileArtifact> {
        let path = self.root.join(name);
        let read = || -> Result<Option<DecompileArtifact>, String> {
            reject_link(&path).map_err(|e| e.to_string())?;
            let file = match File::open(&path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.to_string()),
            };
            let mut bytes = Vec::new();
            (&file)
                .take(ENTRY_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() as u64 > ENTRY_LIMIT {
                return Err("oversized cache entry".into());
            }
            let entry: Entry = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let payload = serialize_bounded(&entry.artifact, ENTRY_LIMIT)?
                .ok_or("cached payload exceeds serialization limit")?;
            if entry.key != *key || entry.artifact_sha256 != sha256_hex(&payload) {
                return Err("cache key or payload checksum mismatch".into());
            }
            if !key.analysis && entry.artifact.upvalue_analysis.is_some() {
                return Err("unexpected cached analysis".into());
            }
            // Best effort recency across processes. Failure to update file
            // timestamps does not invalidate the verified artifact.
            let _ = file.set_times(fs::FileTimes::new().set_modified(SystemTime::now()));
            Ok(Some(entry.artifact))
        };
        match read() {
            Ok(Some(artifact)) => {
                let mut inventory = self.inventory.lock();
                inventory.clock += 1;
                let clock = inventory.clock;
                if let Some(record) = inventory.records.get_mut(name) {
                    let old = record.touched;
                    record.touched = clock;
                    inventory.recency.remove(&(old, name.to_string()));
                    inventory.recency.insert((clock, name.to_string()));
                }
                Some(artifact)
            }
            Ok(None) => None,
            Err(_) => {
                self.counters.corrupt.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    fn write(&self, name: &str, key: Key, artifact: &DecompileArtifact) -> Result<(), String> {
        let limit = ENTRY_LIMIT.min(self.max_bytes);
        let Some(payload) = serialize_bounded(artifact, limit)? else {
            self.counters.bypasses.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        };
        #[derive(Serialize)]
        struct BorrowedEntry<'a> {
            key: Key,
            artifact_sha256: String,
            artifact: &'a DecompileArtifact,
        }
        let Some(bytes) = serialize_bounded(
            &BorrowedEntry {
                key,
                artifact_sha256: sha256_hex(&payload),
                artifact,
            },
            limit,
        )?
        else {
            self.counters.bypasses.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        };
        let size = bytes.len() as u64;
        let path = self.root.join(name);
        reject_link(&path).map_err(|e| e.to_string())?;
        let mut inventory = atomic_write_contained_guarded(&self.root, &path, &bytes, true, || {
            let mut inventory = self.inventory.lock();
            let old_size = inventory.records.get(name).map(|r| r.size).unwrap_or(0);
            let added = usize::from(!inventory.records.contains_key(name));
            while inventory.bytes.saturating_sub(old_size).saturating_add(size) > self.max_bytes
                || inventory.records.len() + added > ENTRY_COUNT_LIMIT
            {
                self.evict_one(&mut inventory, Some(name)).map_err(std::io::Error::other)?;
            }
            reject_link(&path)?;
            Ok(inventory)
        }).map_err(|e| e.to_string())?;
        let old_size = inventory.records.get(name).map(|r| r.size).unwrap_or(0);
        if let Some(old) = inventory.records.get(name).map(|record| record.touched) {
            inventory.recency.remove(&(old, name.to_string()));
        }
        inventory.clock += 1;
        let touched = inventory.clock;
        inventory.recency.insert((touched, name.to_string()));
        inventory
            .records
            .insert(name.to_string(), Record { size, touched });
        inventory.bytes = inventory
            .bytes
            .saturating_sub(old_size)
            .saturating_add(size);
        self.counters.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn evict_one(&self, inventory: &mut Inventory, retain: Option<&str>) -> Result<(), String> {
        let oldest = inventory
            .recency
            .iter()
            .find(|(_, key)| Some(key.as_str()) != retain)
            .map(|(_, name)| name.clone())
            .ok_or("cache quota cannot be satisfied")?;
        let target = self.root.join(&oldest);
        reject_link(&target).map_err(|e| e.to_string())?;
        // Only plain hex names immediately below this canonical marked root.
        // No recursive deletion or path supplied by a serialized entry.
        match fs::remove_file(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let record = inventory.records.remove(&oldest).unwrap();
        inventory.bytes -= record.size;
        inventory.recency.remove(&(record.touched, oldest));
        self.counters.evictions.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn report(&self) -> serde_json::Value {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let inventory = self.inventory.lock();
        serde_json::json!({"schema_version": 1, "model": "executable-context-artifact-cache-v1",
            "executable_sha256": self.executable_sha256, "hits": read(&self.counters.hits),
            "misses": read(&self.counters.misses), "writes": read(&self.counters.writes),
            "corrupt": read(&self.counters.corrupt), "evictions": read(&self.counters.evictions),
            "bypasses": read(&self.counters.bypasses), "io_errors": read(&self.counters.io_errors),
            "raced_hits": read(&self.counters.raced_hits),
            "entries": inventory.records.len(), "bytes": inventory.bytes, "max_bytes": self.max_bytes,
            "entry_byte_limit": ENTRY_LIMIT, "entry_count_limit": ENTRY_COUNT_LIMIT})
    }
}

fn serialize_bounded(value: &impl Serialize, limit: u64) -> Result<Option<Vec<u8>>, String> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: u64,
        exceeded: bool,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len() as u64 + bytes.len() as u64 > self.limit {
                self.exceeded = true;
                return Err(std::io::Error::other("cache serialization limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit,
        exceeded: false,
    };
    match serde_json::to_writer(&mut writer, value) {
        Ok(()) => Ok(Some(writer.bytes)),
        Err(_) if writer.exceeded => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn entry_name(name: &str) -> bool {
    name.strip_suffix(".json").is_some_and(|stem| {
        stem.len() == 64
            && stem
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn reject_link(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cache path is not a regular file",
            ))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "tovek-cache-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("input")).unwrap();
            fs::create_dir_all(root.join("output")).unwrap();
            Self(fs::canonicalize(root).unwrap())
        }
        fn cache(&self, limit: u64) -> Cache {
            Cache::open(
                &self.0.join("cache"),
                limit,
                &self.0.join("input"),
                &self.0.join("output"),
            )
            .unwrap()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            // Literal canonical test-owned directory; no paths from cache data.
            if self.0.parent() == fs::canonicalize(std::env::temp_dir()).ok().as_deref() {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }
    fn artifact(source: &str) -> DecompileArtifact {
        DecompileArtifact {
            source: source.into(),
            upvalue_analysis: None,
        }
    }
    fn defaults() -> DecompileOptions {
        DecompileOptions::default()
    }

    #[test]
    fn complete_context_key_and_normalized_module_paths() {
        let scratch = Scratch::new();
        let mut cache = scratch.cache(4096);
        let a = cache.key(b"input", 203, "A/Widget/init.lua", defaults(), false);
        assert_eq!(
            a,
            cache.key(b"input", 203, "B/Widget.lua", defaults(), false)
        );
        assert_ne!(
            a,
            cache.key(b"input", 203, "A/Gadget/init.lua", defaults(), false)
        );
        assert_ne!(
            a,
            cache.key(b"input!", 203, "A/Widget/init.lua", defaults(), false)
        );
        assert_ne!(
            a,
            cache.key(b"input", 1, "A/Widget/init.lua", defaults(), false)
        );
        assert_ne!(
            a,
            cache.key(b"input", 203, "A/Widget/init.lua", defaults(), true)
        );
        for bits in [1, 2, 4, 8, 16, 32] {
            let options = DecompileOptions::from_flag_bits(bits).unwrap();
            assert_ne!(
                a,
                cache.key(b"input", 203, "A/Widget/init.lua", options, false)
            );
        }
        cache.no_shared_tail = !cache.no_shared_tail;
        assert_ne!(
            a,
            cache.key(b"input", 203, "A/Widget/init.lua", defaults(), false)
        );
        cache.no_shared_tail = !cache.no_shared_tail;
        cache.executable_sha256.push('1');
        assert_ne!(
            a,
            cache.key(b"input", 203, "A/Widget/init.lua", defaults(), false)
        );
    }

    #[test]
    fn disk_hits_validate_checksum_and_corruption_recomputes() {
        let scratch = Scratch::new();
        {
            let cache = scratch.cache(4096);
            cache
                .get_or_compute(b"input", 1, "A/Widget.lua", defaults(), false, || {
                    Ok(artifact("return 7"))
                })
                .unwrap();
        }
        let cache = scratch.cache(4096);
        let hit = cache
            .get_or_compute(b"input", 1, "B/Widget/init.lua", defaults(), false, || {
                panic!("disk entry should be reused")
            })
            .unwrap();
        assert_eq!(hit.source, "return 7");
        let name = cache
            .inventory
            .lock()
            .records
            .keys()
            .next()
            .unwrap()
            .clone();
        let path = cache.root.join(name);
        let mut entry: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        entry["artifact"]["source"] = "return 8".into();
        fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
        let repaired = cache
            .get_or_compute(b"input", 1, "B/Widget/init.lua", defaults(), false, || {
                Ok(artifact("return 7"))
            })
            .unwrap();
        assert_eq!(repaired.source, "return 7");
        assert!(cache.report()["corrupt"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn concurrent_duplicate_context_publishes_once_without_locking_computation() {
        let scratch = Scratch::new();
        let cache = scratch.cache(4096);
        let calls = AtomicU64::new(0);
        std::thread::scope(|scope| {
            for i in 0..8 {
                let cache = &cache;
                let calls = &calls;
                scope.spawn(move || {
                    let result = cache
                        .get_or_compute(
                            b"input",
                            1,
                            &format!("path{i}/Widget.lua"),
                            defaults(),
                            false,
                            || {
                                calls.fetch_add(1, Ordering::Relaxed);
                                Ok(artifact("return 7"))
                            },
                        )
                        .unwrap();
                    assert_eq!(result.source, "return 7");
                });
            }
        });
        let report = cache.report();
        assert_eq!(report["writes"], 1);
        assert_eq!(
            report["hits"].as_u64().unwrap() + report["raced_hits"].as_u64().unwrap(),
            7
        );
        assert!(calls.load(Ordering::Relaxed) <= 8);
    }

    #[test]
    fn failures_and_oversized_payloads_do_not_create_entries() {
        let scratch = Scratch::new();
        let cache = scratch.cache(1024);
        assert!(cache
            .get_or_compute(b"input", 1, "Widget", defaults(), false, || Err(
                "transient failure".into()
            ))
            .is_err());
        let large = "x".repeat(2048);
        let result = cache
            .get_or_compute(b"input", 1, "Widget", defaults(), false, || {
                Ok(artifact(&large))
            })
            .unwrap();
        assert_eq!(result.source, large);
        assert_eq!(cache.report()["entries"], 0);
        assert_eq!(cache.report()["bypasses"], 1);
        assert!(serialize_bounded(&"\n".repeat(100), 100).unwrap().is_none());
    }

    #[test]
    fn bounded_lru_evicts_entries_but_retains_recent_hits() {
        let scratch = Scratch::new();
        let cache = scratch.cache(2000);
        for input in [b"a", b"b"] {
            cache
                .get_or_compute(input, 1, "Widget", defaults(), false, || {
                    Ok(artifact(&"x".repeat(300)))
                })
                .unwrap();
        }
        cache
            .get_or_compute(b"a", 1, "Widget", defaults(), false, || {
                panic!("expected hit")
            })
            .unwrap();
        cache
            .get_or_compute(b"c", 1, "Widget", defaults(), false, || {
                Ok(artifact(&"x".repeat(300)))
            })
            .unwrap();
        assert!(cache.report()["bytes"].as_u64().unwrap() <= 2000);
        assert!(cache.report()["evictions"].as_u64().unwrap() > 0);
        cache
            .get_or_compute(b"a", 1, "Widget", defaults(), false, || {
                panic!("recent entry must remain")
            })
            .unwrap();
    }

    #[test]
    fn lock_and_tree_overlap_refuse_unsafe_cache_placement() {
        let scratch = Scratch::new();
        let cache = scratch.cache(4096);
        assert!(Cache::open(
            &cache.root,
            4096,
            &scratch.0.join("input"),
            &scratch.0.join("output")
        )
        .is_err());
        assert!(Cache::open(
            &scratch.0.join("input/cache"),
            4096,
            &scratch.0.join("input"),
            &scratch.0.join("output")
        )
        .is_err());
    }

    #[test]
    fn unknown_cache_contents_are_never_adopted_for_eviction() {
        let scratch = Scratch::new();
        let root = scratch.0.join("cache");
        fs::create_dir(&root).unwrap();
        let unknown = root.join(format!("{}.json", "a".repeat(64)));
        fs::write(&unknown, b"user data").unwrap();
        assert!(Cache::open(
            &root,
            1,
            &scratch.0.join("input"),
            &scratch.0.join("output")
        )
        .is_err());
        assert_eq!(fs::read(&unknown).unwrap(), b"user data");
        assert!(!root.join(".tovek-cache-v1").exists());
        fs::write(root.join(".tovek-cache-v1"), vec![0; 1024]).unwrap();
        assert!(Cache::open(
            &root,
            1,
            &scratch.0.join("input"),
            &scratch.0.join("output")
        )
        .is_err());
        assert_eq!(fs::read(unknown).unwrap(), b"user data");
    }

    #[test]
    fn reducing_quota_applies_before_any_cache_hit() {
        let scratch = Scratch::new();
        {
            let cache = scratch.cache(4096);
            cache
                .get_or_compute(b"a", 1, "Widget", defaults(), false, || {
                    Ok(artifact(&"x".repeat(500)))
                })
                .unwrap();
            assert!(cache.report()["bytes"].as_u64().unwrap() > 512);
        }
        let cache = scratch.cache(512);
        assert_eq!(cache.report()["bytes"], 0);
        assert_eq!(cache.report()["entries"], 0);
        assert_eq!(cache.report()["evictions"], 1);
    }

    #[test]
    fn analysis_roundtrip_preserves_serialization_and_owned_diagnostics() {
        let scratch = Scratch::new();
        let cache = scratch.cache(1024 * 1024);
        let bytes = include_bytes!("../tests/fixtures/upvalue_analysis_nested_g0.luaubc");
        let mut fresh = luau_lifter::try_decompile_bytecode_artifact_with_options(
            bytes,
            1,
            Some("A/Widget.lua"),
            defaults(),
        )
        .unwrap();
        fresh.upvalue_analysis.as_mut().unwrap().diagnostics.push(
            luau_lifter::upvalue_analysis::AnalysisDiagnostic {
                code: "test_owned_code".into(),
                message: "preserved diagnostic".into(),
                proto_id: None,
                pc: None,
            },
        );
        let expected = serde_json::to_vec_pretty(&fresh).unwrap();
        cache
            .get_or_compute(bytes, 1, "A/Widget.lua", defaults(), true, || Ok(fresh))
            .unwrap();
        let cached = cache
            .get_or_compute(bytes, 1, "B/Widget.lua", defaults(), true, || {
                panic!("analysis should hit")
            })
            .unwrap();
        assert_eq!(serde_json::to_vec_pretty(&cached).unwrap(), expected);
    }

    #[test]
    fn folder_cache_regenerates_per_path_metadata_and_matches_uncached_output() {
        use base64::Engine;
        let scratch = Scratch::new();
        let input = scratch.0.join("input");
        let bytes = include_bytes!("../tests/fixtures/cache_context.luaubc");
        let encoded = base64::prelude::BASE64_STANDARD.encode(bytes);
        for name in ["A/Widget.lua", "B/Widget/init.lua", "C/Gadget.lua"] {
            let path = input.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, &encoded).unwrap();
        }
        let options = DecompileOptions {
            emit_binding_provenance: true,
            ..defaults()
        };
        for (label, cached, threads) in [("plain", false, 1), ("cold", true, 4), ("warm", true, 1)]
        {
            let output = scratch.0.join(label);
            let cache_path = scratch.0.join("cache");
            assert_eq!(
                crate::batch::run_with_cache(
                    &input,
                    &output,
                    1,
                    threads,
                    false,
                    options,
                    true,
                    "luau",
                    None,
                    cached.then_some(cache_path.as_path()),
                    16
                ),
                0
            );
        }
        let entries = fs::read_dir(scratch.0.join("cache"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| entry_name(&e.file_name().to_string_lossy()))
            .count();
        assert_eq!(
            entries, 2,
            "same module context should deduplicate across paths"
        );
        let baseline = scratch.0.join("plain");
        assert_ne!(
            fs::read(baseline.join("A/Widget.luau")).unwrap(),
            fs::read(baseline.join("C/Gadget.luau")).unwrap(),
            "same bytecode with different module contexts is a real counterexample"
        );
        for item in walkdir::WalkDir::new(&baseline)
            .into_iter()
            .map(Result::unwrap)
            .filter(|e| e.file_type().is_file())
        {
            let relative = item.path().strip_prefix(&baseline).unwrap();
            // Generation locks have no source/metadata content contract.
            if relative.to_string_lossy().ends_with(".lock") {
                continue;
            }
            for label in ["cold", "warm"] {
                assert_eq!(
                    fs::read(item.path()).unwrap(),
                    fs::read(scratch.0.join(label).join(relative)).unwrap(),
                    "{label}: {}",
                    relative.display()
                );
            }
        }
    }
}
