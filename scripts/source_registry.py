#!/usr/bin/env python3
"""Build and query an offline, pinned source registry without changing decompilation.

Matches require a full execution-image comparison and fresh source recompilation.
Source text ambiguity and low-information chunks always refuse selection.
"""
import argparse
import base64
import binascii
import collections
import concurrent.futures
import hashlib
import json
import pathlib
import re
import subprocess
import tempfile
import time
import threading

from bytecode_roundtrip import BytecodeError
from roadmap_v2 import ROOT, fixture_path, sha256
from source_fingerprint import MAX_BYTES, MODEL, execution_image

MAX_SOURCES = 2000
MAX_PROFILES = 8
MAX_REGISTRY_BYTES = 64 * 1024 * 1024
COMPILER_COMMIT = 'c2ec0d4e5ca50796ba174a7565298f59aa572268'
LABEL = b'-- Tovek: matched upstream source; commit and license are recorded in the sibling metadata file.\n'
STORE_LOCK = threading.RLock()


def encoded(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True) + '\n').encode()


def digest(value):
    return hashlib.sha256(value).hexdigest()


def write_once(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    # Several profiles can generate the same artifact concurrently. Readers of
    # an existing content-addressed file must not observe an unfinished write.
    with STORE_LOCK:
        try:
            with path.open('xb') as stream:
                stream.write(data)
        except FileExistsError:
            if read_file(path, max(len(data), 1)) != data:
                raise ValueError(f'refusing to overwrite different artifact: {path}')


def read_file(path, limit=MAX_BYTES):
    with path.open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError('artifact byte budget exceeded')
    return data


def read_input(path, saved=False):
    data = read_file(path, MAX_BYTES * 2 if saved else MAX_BYTES)
    if not saved:
        return data, digest(data)
    try:
        text = data.decode('utf-8')
        body = ''.join(line.strip() for line in text.splitlines() if not line.lstrip().startswith('--'))
        raw = base64.b64decode(body, validate=True) if body else b''
    except (UnicodeDecodeError, binascii.Error, ValueError) as error:
        raise ValueError('saved input is not valid UTF-8/base64') from error
    if len(raw) > MAX_BYTES:
        raise ValueError('decoded bytecode byte budget exceeded')
    return raw, digest(data)


def store(root, kind, data, suffix):
    relative = f'{kind}/{digest(data)}{suffix}'
    # Serialize containment checks with first directory/file creation so other
    # workers cannot change a path while it is being resolved on Windows.
    with STORE_LOCK:
        root.mkdir(parents=True, exist_ok=True)
        fixture_path(root, kind).mkdir(parents=True, exist_ok=True)
        write_once(fixture_path(root, relative), data)
    return relative


def validate_profile(profile):
    if not re.fullmatch(r'[a-z0-9_]+', profile['id']) or profile['opt'] not in (0, 1, 2) or \
            profile['debug'] != 1 or profile['type_info'] not in (0, 1):
        raise ValueError('unsupported compiler profile')
    if profile['source_preamble'] not in ('', '--!native\n'):
        raise ValueError('unsupported compiler source preamble')
    base = ['--fflags=false']
    vector = base + ['--vector-lib=Vector3', '--vector-ctor=new', '--vector-type=Vector3']
    if profile['flags'] not in (base, vector):
        raise ValueError('unsupported compiler flags')


def compile_bytes(compiler, source, profile, *, preamble=True):
    validate_profile(profile)
    if len(source) > MAX_BYTES:
        raise ValueError('source exceeds compiler input budget')
    with tempfile.TemporaryDirectory(prefix='tovek_registry_') as temporary:
        path = pathlib.Path(temporary) / 'input.luau'
        path.write_bytes((profile['source_preamble'].encode() if preamble else b'') + source)
        result = subprocess.run([str(compiler), '--binary', f"-O{profile['opt']}", f"-g{profile['debug']}",
                                 f"-t{profile['type_info']}", *profile['flags'], str(path)],
                                capture_output=True, timeout=30)
    if result.returncode:
        raise ValueError('source recompile failed: ' + result.stderr.decode(errors='replace')[:1000])
    if len(result.stdout) > MAX_BYTES:
        raise ValueError('compiled bytecode exceeds image budget')
    return result.stdout


def build(args):
    config = json.loads(read_file(args.config))
    if config['schema_version'] != 1 or config['fingerprint_model'] != MODEL or config['compiler_commit'] != COMPILER_COMMIT:
        raise ValueError('unsupported registry config')
    if config['admission']['minimum_instructions'] != 8 or config['admission']['minimum_substantive_instructions'] != 4:
        raise ValueError('admission thresholds do not match the versioned fingerprint model')
    manifest_path = fixture_path(args.config.parent, config['source_manifest'])
    manifest = json.loads(read_file(manifest_path))
    profiles = config['profiles']
    if not 0 < len(profiles) <= MAX_PROFILES or len({p['id'] for p in profiles}) != len(profiles):
        raise ValueError('duplicate or excessive compiler profiles')
    for profile in profiles:
        validate_profile(profile)
    sources = manifest['sources']
    if manifest['schema_version'] != 1 or manifest['compiler_commit'] != COMPILER_COMMIT or not 0 < len(sources) <= MAX_SOURCES:
        raise ValueError('unsupported source manifest')
    repos = {}
    for repo in manifest['repositories']:
        if repo['name'] in repos or not re.fullmatch(r'[0-9a-f]{40}', repo['commit']):
            raise ValueError('duplicate repository or invalid commit')
        path = fixture_path(args.vendor, repo['name'])
        head = subprocess.check_output(['git', '-C', str(path), 'rev-parse', 'HEAD'], timeout=30).decode().strip()
        if head != repo['commit']:
            raise ValueError('repository commit mismatch: ' + repo['name'])
        license_ = repo['license']
        text = read_file(fixture_path(path, license_['path']))
        if digest(text) != license_['sha256'] or not license_['spdx'] or len(text) > MAX_BYTES:
            raise ValueError('license hash or identity mismatch')
        repos[repo['name']] = dict(repo, license=dict(license_, artifact=store(args.registry, 'licenses', text, '.txt')))
    jobs, seen, source_bytes = [], set(), 0
    for entry in sources:
        identity = entry['repo'], entry['file']
        if identity in seen or entry['repo'] not in repos:
            raise ValueError('duplicate or unknown source identity')
        seen.add(identity)
        source = read_file(fixture_path(args.vendor / entry['repo'], entry['file']))
        source_bytes += len(source)
        if digest(source) != entry['source_sha256'] or len(source) > MAX_BYTES or source_bytes > MAX_REGISTRY_BYTES:
            raise ValueError('source hash or size budget mismatch')
        artifact = store(args.registry, 'sources', source, '.luau')
        for profile in profiles:
            jobs.append((entry, source, artifact, profile))
    def process(job):
        entry, source, artifact, profile = job
        raw = compile_bytes(args.compiler, source, profile)
        image, facts = execution_image(raw)
        repo = repos[entry['repo']]
        row = dict(repo=entry['repo'], file=entry['file'], url=entry['url'], commit=repo['commit'],
                   split=entry['split'], lineage=entry['lineage'], source_sha256=entry['source_sha256'],
                   source_artifact=artifact, license=repo['license'], profile=profile['id'],
                   bytecode_sha256=digest(raw), bytecode_artifact=store(args.registry, 'bytecode', raw, '.luaubc'),
                   image_sha256=digest(image), facts=facts)
        return dict(id=digest(encoded(row)), **row)
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        rows = list(pool.map(process, jobs))
    rows.sort(key=lambda row: (row['repo'], row['file'], row['profile']))
    index = dict(schema_version=1, model=MODEL, compiler_commit_expected=COMPILER_COMMIT,
                 compiler_sha256=sha256(args.compiler), config_sha256=sha256(args.config),
                 source_manifest_sha256=sha256(manifest_path), profiles=profiles, entries=rows,
                 contract=config['admission'], source_files=len(sources), source_bytes=source_bytes)
    index['registry_id'] = digest(encoded(index))
    write_once(args.registry / 'index.json', encoded(index))
    return dict(registry_id=index['registry_id'], source_files=len(sources), profiles=len(profiles), entries=len(rows),
                low_information_entries=sum(row['facts']['low_information'] for row in rows))


class Registry:
    def __init__(self, root, compiler):
        self.root, self.compiler = root, compiler
        raw_index = read_file(root / 'index.json', MAX_REGISTRY_BYTES)
        if len(raw_index) > MAX_REGISTRY_BYTES:
            raise ValueError('registry index byte budget exceeded')
        self.index = json.loads(raw_index)
        index = self.index
        if index['schema_version'] != 1 or index['model'] != MODEL or index['compiler_sha256'] != sha256(compiler) or \
                index['compiler_commit_expected'] != COMPILER_COMMIT:
            raise ValueError('registry compiler/model mismatch; rebuild with the intended compiler')
        if digest(encoded({k: v for k, v in index.items() if k != 'registry_id'})) != index['registry_id']:
            raise ValueError('registry index identity mismatch')
        self.profiles = {p['id']: p for p in index['profiles']}
        if not 0 < len(self.profiles) == len(index['profiles']) <= MAX_PROFILES:
            raise ValueError('registry profile budget or identity mismatch')
        for profile in self.profiles.values():
            validate_profile(profile)
        if not 0 < len(index['entries']) <= MAX_SOURCES * MAX_PROFILES:
            raise ValueError('registry entry budget exceeded')
        self.images, self.by_image, self.blobs, self.verified = {}, collections.defaultdict(list), {}, {}
        image_cache = {}
        total, image_bytes = 0, 0
        for row in index['entries']:
            if row['id'] in self.images or row['profile'] not in self.profiles or \
                    digest(encoded({k: v for k, v in row.items() if k != 'id'})) != row['id']:
                raise ValueError('registry row identity mismatch')
            for path_key, hash_key in [('bytecode_artifact', 'bytecode_sha256'), ('source_artifact', 'source_sha256')]:
                relative = row[path_key]
                if relative not in self.blobs:
                    blob = read_file(fixture_path(root, relative))
                    total += len(blob)
                    if len(blob) > MAX_BYTES or total > MAX_REGISTRY_BYTES:
                        raise ValueError('registry artifact byte budget exceeded')
                    self.blobs[relative] = blob
                if digest(self.blobs[relative]) != row[hash_key]:
                    raise ValueError('registry artifact hash mismatch')
            license_ = row['license']
            license_path = license_['artifact']
            if license_path not in self.blobs:
                blob = read_file(fixture_path(root, license_path))
                total += len(blob)
                if len(blob) > MAX_BYTES or total > MAX_REGISTRY_BYTES:
                    raise ValueError('license byte budget exceeded')
                self.blobs[license_path] = blob
            if not license_['spdx'] or digest(self.blobs[license_path]) != license_['sha256']:
                raise ValueError('registry license mismatch')
            if row['bytecode_sha256'] not in image_cache:
                image, facts = execution_image(self.blobs[row['bytecode_artifact']])
                image_bytes += len(image)
                if image_bytes > MAX_REGISTRY_BYTES * 2:
                    raise ValueError('registry canonical-image budget exceeded')
                image_cache[row['bytecode_sha256']] = image, facts
            image, facts = image_cache[row['bytecode_sha256']]
            if digest(image) != row['image_sha256'] or facts != row['facts']:
                raise ValueError('registry execution image mismatch')
            self.images[row['id']] = image
            self.by_image[row['image_sha256']].append(row)

    def match(self, raw, key=1, trailer_bytes=0):
        image, facts = execution_image(raw, key, trailer_bytes)
        candidates = [row for row in self.by_image.get(digest(image), ()) if self.images[row['id']] == image]
        result = dict(bytecode_sha256=digest(raw), image_sha256=digest(image), input_facts=facts,
                      candidate_entries=len(candidates), candidate_source_texts=len({r['source_sha256'] for r in candidates}))
        if facts['low_information']:
            return dict(result, status='refused_low_information')
        if not candidates:
            return dict(result, status='no_match')
        if result['candidate_source_texts'] != 1:
            return dict(result, status='refused_ambiguous_source_text', candidates=[r['id'] for r in candidates])
        if len(candidates) > 64:
            return dict(result, status='refused_candidate_budget')
        accepted = []
        for row in candidates:
            if row['id'] not in self.verified:
                compiled = compile_bytes(self.compiler, self.blobs[row['source_artifact']], self.profiles[row['profile']])
                self.verified[row['id']] = execution_image(compiled)[0] == image
            if not self.verified[row['id']]:
                return dict(result, status='refused_source_recompile_mismatch')
            accepted.append(dict(row, known_differences=dict(
                debug_line_or_local_metadata=facts['debug_metadata'] != row['facts']['debug_metadata'],
                compiler_source_preamble=self.profiles[row['profile']]['source_preamble'],
                opaque_container_trailer_bytes=facts['opaque_trailer_bytes'])))
        return dict(result, status='matched_upstream_source', matches=accepted)

    def materialize(self, result, relative, output):
        if result['status'] != 'matched_upstream_source':
            return None
        row = result['matches'][0]
        profile = self.profiles[row['profile']]
        source = profile['source_preamble'].encode() + LABEL + self.blobs[row['source_artifact']]
        rebuilt = compile_bytes(self.compiler, source, profile, preamble=False)
        if execution_image(rebuilt)[0] != self.images[row['id']]:
            raise ValueError('labelled source changed the verified execution image')
        path = fixture_path(output, pathlib.Path(relative).with_suffix('.matched.luau'))
        write_once(path, source)
        license_paths = []
        for match in result['matches']:
            license_ = match['license']
            license_paths.append(store(output, 'licenses', self.blobs[license_['artifact']], '.txt'))
        metadata = dict(classification='matched upstream source', registry_id=self.index['registry_id'],
                        output_sha256=digest(source), original_upstream_source_sha256=row['source_sha256'],
                        known_additions=dict(source_preamble=profile['source_preamble'], registry_label=LABEL.decode()),
                        license_artifacts=sorted(set(license_paths)), verification=result)
        write_once(path.with_suffix('.json'), encoded(metadata))
        return path.relative_to(output.resolve()).as_posix()


def query(args):
    registry = Registry(args.registry, args.compiler)
    paths = sorted(args.corpus.rglob('*.lua')) if args.corpus else [args.bytecode]
    if len(paths) > 100_000:
        raise ValueError('query file budget exceeded')
    rows = []
    for path in paths:
        relative = path.relative_to(args.corpus).as_posix() if args.corpus else path.name
        started = time.perf_counter()
        artifact_hash = None
        try:
            raw, artifact_hash = read_input(path, bool(args.corpus or args.saved))
            if not raw:
                row = dict(status='empty_input')
            else:
                row = registry.match(raw, args.key, args.trailer_bytes)
            if args.materialize and row['status'] == 'matched_upstream_source':
                row['materialized_path'] = registry.materialize(row, relative, args.materialize)
        except (ValueError, OSError, BytecodeError, subprocess.SubprocessError) as error:
            row = dict(status='refused_input_or_verification', error=str(error))
        rows.append(dict(file=relative, input_artifact_sha256=artifact_hash, seconds=time.perf_counter() - started, **row))
    matches = [r for r in rows if r['status'] == 'matched_upstream_source']
    coverage = collections.Counter()
    for row in matches:
        coverage.update({m['repo'] for m in row['matches']})
    report = dict(schema_version=1, registry_id=registry.index['registry_id'], compiler_sha256=sha256(args.compiler),
                  input_key=args.key, allowed_opaque_trailer_bytes=sorted({0, args.trailer_bytes}),
                  summary=dict(total=len(rows), status=dict(collections.Counter(r['status'] for r in rows)),
                               matched_files_by_upstream_family=dict(coverage),
                               unmatched_or_refused_nonempty_files=sum(r['status'] not in ('matched_upstream_source', 'empty_input') for r in rows)),
                  rows=rows, contract=registry.index['contract'],
                  limitations='Matched source is compatible with the exact retained v9 execution image under the recorded compiler profile. Original source text, debug line/stack reflection, opaque transport authentication and module availability at runtime are not established. Unmatched code is not automatically classified as custom. Ordinary decompilation is unchanged.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    return report['summary']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    builder = sub.add_parser('build')
    builder.add_argument('--config', type=pathlib.Path, default=ROOT / 'docs/source_registry_v2.json')
    builder.add_argument('--vendor', type=pathlib.Path, required=True)
    matcher = sub.add_parser('query')
    inputs = matcher.add_mutually_exclusive_group(required=True)
    inputs.add_argument('--corpus', type=pathlib.Path)
    inputs.add_argument('--bytecode', type=pathlib.Path)
    matcher.add_argument('--saved', action='store_true')
    matcher.add_argument('--key', type=int, default=1)
    matcher.add_argument('--trailer-bytes', type=int, choices=(0, 24), default=0,
                         help='explicitly allow this opaque trailer size in addition to a bare serialized chunk')
    matcher.add_argument('--materialize', type=pathlib.Path)
    matcher.add_argument('--report', type=pathlib.Path, required=True)
    for item in (builder, matcher):
        item.add_argument('--registry', type=pathlib.Path, required=True)
        item.add_argument('--compiler', type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.compiler = args.compiler.resolve(strict=True)
    try:
        summary = build(args) if args.command == 'build' else query(args)
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        parser.error(str(error))
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
