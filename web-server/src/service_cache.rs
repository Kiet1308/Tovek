//! Optional process-local source cache and non-blocking duplicate coordination.
//!
//! The running executable is the cache namespace: nothing survives a process or
//! build change. Keys compare exact bytecode, every option, the decode key, full
//! naming context and the one supported semantic environment switch. A full
//! script path is intentionally more conservative than a normalized module hint.
//! No mutex is held during CPU work or while a duplicate waits; in particular,
//! these locks are never acquired around nested Rayon computation.
use crate::Error;
use axum::body::Bytes;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub(super) type Outcome = Result<Bytes, Error>;
pub(super) type Receiver = watch::Receiver<Option<Outcome>>;

const MAX_ENTRIES: usize = 1024;
const MAX_PENDING: usize = 64;
const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Eq, PartialEq)]
pub(super) struct Key {
    hash: u64,
    bytecode: Bytes,
    decode_key: u8,
    options: u32,
    script_name: Option<Arc<str>>,
    no_shared_tail: bool,
}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // The expensive bytecode hash is prepared on the blocking parser pool.
        // Equality still checks every byte, so a hash collision is only a miss.
        state.write_u64(self.hash);
    }
}

impl Key {
    pub fn new(bytecode: &[u8], decode_key: u8, options: u32,
               script_name: Option<&str>, no_shared_tail: bool) -> Self {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        (bytecode, decode_key, options, script_name, no_shared_tail).hash(&mut hash);
        Self {
            hash: hash.finish(),
            // A Bytes slice from MDB1 can otherwise pin an entire 64 MiB body.
            bytecode: Bytes::copy_from_slice(bytecode),
            decode_key,
            options,
            script_name: script_name.map(Into::into),
            no_shared_tail,
        }
    }

    fn bytes(&self) -> usize {
        self.bytecode.len().saturating_add(self.script_name.as_ref().map_or(0, |name| name.len()))
            .saturating_add(256) // bounded bookkeeping allowance, in addition to entry caps
    }
}

struct Ready {
    source: Bytes,
    bytes: usize,
    touched: u64,
}

struct Pending {
    id: u64,
    receiver: Receiver,
    bytes: usize,
}

#[derive(Default)]
struct State {
    ready: HashMap<Key, Ready>,
    pending: HashMap<Key, Pending>,
    ready_bytes: usize,
    pending_bytes: usize,
    clock: u64,
}

struct Inner {
    max_bytes: usize,
    max_entry_bytes: usize,
    max_entries: usize,
    max_pending: usize,
    max_pending_bytes: usize,
    state: Mutex<State>,
}

#[derive(Clone)]
pub(super) struct SourceCache(Arc<Inner>);

pub(super) enum Lookup {
    Ready(Bytes),
    Wait(Receiver),
    Lead(Receiver, Completion),
    Bypass,
}

impl SourceCache {
    pub fn new(max_bytes: usize) -> Self {
        Self(Arc::new(Inner {
            max_bytes,
            max_entry_bytes: MAX_ENTRY_BYTES.min(max_bytes),
            max_entries: MAX_ENTRIES,
            max_pending: MAX_PENDING,
            max_pending_bytes: MAX_PENDING_BYTES.min(max_bytes),
            state: Mutex::new(State::default()),
        }))
    }

    pub fn enabled(&self) -> bool { self.0.max_bytes != 0 }

    #[cfg(test)]
    pub fn counts(&self) -> (usize, usize) {
        let state = self.0.state.lock().unwrap();
        (state.ready.len(), state.pending.len())
    }

    pub fn accepts_key(&self, bytecode_len: usize, name_len: usize) -> bool {
        self.enabled() && bytecode_len.saturating_add(name_len).saturating_add(256)
            <= self.0.max_entry_bytes.min(self.0.max_pending_bytes)
    }

    pub fn claim(&self, key: Key) -> Lookup {
        if !self.enabled() { return Lookup::Bypass; }
        let bytes = key.bytes();
        let mut state = self.0.state.lock().unwrap_or_else(|error| error.into_inner());
        state.clock = state.clock.wrapping_add(1);
        let clock = state.clock;
        if let Some(entry) = state.ready.get_mut(&key) {
            entry.touched = clock;
            return Lookup::Ready(entry.source.clone());
        }
        if let Some(entry) = state.pending.get(&key) {
            return Lookup::Wait(entry.receiver.clone());
        }
        if bytes > self.0.max_entry_bytes
            || state.pending.len() >= self.0.max_pending
            || bytes > self.0.max_pending_bytes.saturating_sub(state.pending_bytes)
        {
            return Lookup::Bypass;
        }
        let (sender, receiver) = watch::channel(None);
        state.pending_bytes += bytes;
        state.pending.insert(key.clone(), Pending { id: clock, receiver: receiver.clone(), bytes });
        Lookup::Lead(receiver, Completion { cache: self.clone(), key, id: clock, sender, complete: false })
    }
}

/// Owned by the detached computation, never by the requesting HTTP future.
/// Dropping a failed/cancelled worker wakes all followers and removes its key.
pub(super) struct Completion {
    cache: SourceCache,
    key: Key,
    id: u64,
    sender: watch::Sender<Option<Outcome>>,
    complete: bool,
}

impl Completion {
    pub fn finish(mut self, outcome: Outcome) {
        {
            let inner = &self.cache.0;
            let mut state = inner.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.pending.get(&self.key).is_some_and(|pending| pending.id == self.id) {
                let pending = state.pending.remove(&self.key).unwrap();
                state.pending_bytes -= pending.bytes;
                // Errors, including transient admission errors and panics, are
                // delivered to current followers but never become ready entries.
                if let Ok(source) = &outcome {
                    let bytes = self.key.bytes().saturating_add(source.len());
                    if bytes <= inner.max_entry_bytes && bytes <= inner.max_bytes {
                        while state.ready.len() >= inner.max_entries
                            || bytes > inner.max_bytes.saturating_sub(state.ready_bytes)
                        {
                            let Some(oldest) = state.ready.iter().min_by_key(|(_, entry)| entry.touched)
                                .map(|(key, _)| key.clone()) else { break; };
                            state.ready_bytes -= state.ready.remove(&oldest).unwrap().bytes;
                        }
                        state.clock = state.clock.wrapping_add(1);
                        let touched = state.clock;
                        state.ready.insert(self.key.clone(), Ready { source: source.clone(), bytes, touched });
                        state.ready_bytes += bytes;
                    }
                }
            }
        }
        self.sender.send_replace(Some(outcome));
        self.complete = true;
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        if self.complete { return; }
        {
            let mut state = self.cache.0.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.pending.get(&self.key).is_some_and(|pending| pending.id == self.id) {
                let pending = state.pending.remove(&self.key).unwrap();
                state.pending_bytes -= pending.bytes;
            }
        }
        self.sender.send_replace(Some(Err(Error::Io(std::io::Error::other("decompile worker stopped")))));
    }
}

pub(super) async fn wait(mut receiver: Receiver) -> Outcome {
    loop {
        if let Some(outcome) = receiver.borrow_and_update().clone() { return outcome; }
        receiver.changed().await.map_err(|_| Error::Io(std::io::Error::other("decompile worker stopped")))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(bytes: &[u8]) -> Key { Key::new(bytes, 203, 0, Some("Widget"), false) }
    fn leader(cache: &SourceCache, key: Key) -> (Receiver, Completion) {
        match cache.claim(key) { Lookup::Lead(receiver, completion) => (receiver, completion), _ => panic!("expected leader") }
    }

    #[tokio::test]
    async fn concurrent_duplicates_have_one_owner_and_keep_output_after_caller_cancellation() {
        let cache = SourceCache::new(4096);
        let (first, completion) = leader(&cache, key(b"code"));
        let mut followers = Vec::new();
        for _ in 0..8 {
            match cache.claim(key(b"code")) {
                Lookup::Wait(receiver) => followers.push(receiver), _ => panic!("duplicate became leader"),
            }
        }
        drop(first); // the original requester has disconnected
        completion.finish(Ok(Bytes::from_static(b"return 7")));
        for receiver in followers { assert_eq!(wait(receiver).await.unwrap(), "return 7"); }
        assert!(matches!(cache.claim(key(b"code")), Lookup::Ready(_)));
        assert!(cache.0.state.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn failed_or_dropped_workers_release_keys_and_wake_followers_without_caching_errors() {
        let cache = SourceCache::new(4096);
        for drop_worker in [false, true] {
            let (receiver, completion) = leader(&cache, key(b"code"));
            if drop_worker { drop(completion); }
            else { completion.finish(Err(Error::Unavailable("busy"))); }
            assert!(tokio::time::timeout(std::time::Duration::from_secs(1), wait(receiver)).await.unwrap().is_err());
            assert!(cache.0.state.lock().unwrap().ready.is_empty());
            assert_eq!(cache.0.state.lock().unwrap().pending_bytes, 0);
        }
        let (_, completion) = leader(&cache, key(b"code"));
        completion.finish(Ok(Bytes::from_static(b"recovered")));
        assert!(matches!(cache.claim(key(b"code")), Lookup::Ready(_)));
    }

    #[test]
    fn every_semantic_context_is_part_of_the_key_and_collisions_compare_bytes() {
        let original = key(b"code");
        for changed in [
            Key::new(b"code!", 203, 0, Some("Widget"), false),
            Key::new(b"code", 1, 0, Some("Widget"), false),
            Key::new(b"code", 203, 1, Some("Widget"), false),
            Key::new(b"code", 203, 0, Some("Gadget"), false),
            Key::new(b"code", 203, 0, None, false),
            Key::new(b"code", 203, 0, Some("Widget"), true),
        ] { assert!(original != changed); }
        let mut collision = key(b"different");
        collision.hash = original.hash;
        let cache = SourceCache::new(4096);
        let (_, completion) = leader(&cache, original);
        completion.finish(Ok(Bytes::from_static(b"first")));
        assert!(matches!(cache.claim(collision), Lookup::Lead(..)));
    }

    #[test]
    fn ready_and_inflight_memory_are_bounded_and_lru_hits_are_retained() {
        let cache = SourceCache::new(800);
        for input in [b"a", b"b"] {
            let (_, completion) = leader(&cache, key(input));
            completion.finish(Ok(Bytes::from(vec![b'x'; 100])));
        }
        assert!(matches!(cache.claim(key(b"a")), Lookup::Ready(_)));
        let (_, completion) = leader(&cache, key(b"c"));
        completion.finish(Ok(Bytes::from(vec![b'x'; 100])));
        assert!(matches!(cache.claim(key(b"a")), Lookup::Ready(_)));
        assert!(cache.0.state.lock().unwrap().ready_bytes <= 800);
        let (_, pending1) = leader(&cache, key(b"pending1"));
        let (_, pending2) = leader(&cache, key(b"pending2"));
        assert!(matches!(cache.claim(key(b"pending3")), Lookup::Bypass));
        assert!(matches!(cache.claim(key(b"pending1")), Lookup::Wait(_)));
        drop((pending1, pending2));
        assert_eq!(cache.0.state.lock().unwrap().pending_bytes, 0);
    }

    #[test]
    fn disabled_or_oversized_cache_never_retains_inputs() {
        let cache = SourceCache::new(0);
        assert!(!cache.accepts_key(1, 0));
        assert!(matches!(cache.claim(key(b"x")), Lookup::Bypass));
        let cache = SourceCache::new(512);
        assert!(!cache.accepts_key(513, 0));
        let (_, completion) = leader(&cache, key(b"x"));
        completion.finish(Ok(Bytes::from(vec![b'x'; 512])));
        assert!(cache.0.state.lock().unwrap().ready.is_empty());
    }
}
