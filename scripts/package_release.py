#!/usr/bin/env python3
"""Smoke-test and package an explicit allowlist of public release files."""
import argparse
import hashlib
import base64
import json
import pathlib
import re
import shutil
import subprocess
import socket
import tarfile
import tempfile
import time
import urllib.request
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]


def smoke_server(server, bytecode, expected):
    # Never terminate or reuse an unrelated local server.
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 3000))
    with tempfile.TemporaryFile() as log:
        process = subprocess.Popen([str(server)], stdout=log, stderr=log,
                                   creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        try:
            for _ in range(100):
                assert process.poll() is None, 'server exited during startup'
                try:
                    with socket.create_connection(('127.0.0.1', 3000), timeout=.1):
                        break
                except OSError:
                    time.sleep(.1)
            else:
                raise RuntimeError('server startup timed out')
            request = urllib.request.Request('http://127.0.0.1:3000/decompile/raw', data=bytecode,
                                             headers={'X-Encode-Key': '1'})
            with urllib.request.urlopen(request, timeout=30) as response:
                assert response.read().strip() == expected.strip(), 'raw decompilation differs from CLI'
            body = json.dumps({'key': 1, 'scripts': [
                {'id': 'valid', 'bytecode': base64.b64encode(bytecode).decode()},
                {'id': 'invalid', 'bytecode': base64.b64encode(b'bad').decode()}]}).encode()
            request = urllib.request.Request('http://127.0.0.1:3000/decompile/batch', data=body,
                                             headers={'Content-Type': 'application/json'})
            with urllib.request.urlopen(request, timeout=30) as response:
                result = json.load(response)
            assert result['count'] == 2 and result['ok_count'] == 1, result
            assert result['results'][0]['ok'] and not result['results'][1]['ok'], result
        finally:
            process.terminate()
            process.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--platform', choices=('windows-x86_64', 'linux-x86_64'), required=True)
    parser.add_argument('--bin-dir', type=pathlib.Path, required=True)
    parser.add_argument('--out', type=pathlib.Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'v2-v\d+\.\d+(?:\.\d+)?', args.tag):
        parser.error('expected a V2 release tag such as v2-v0.1')
    version = args.tag.removeprefix('v2-')
    suffix = '.exe' if args.platform.startswith('windows') else ''
    cli = (args.bin_dir / ('luau-lifter' + suffix)).resolve()
    server = (args.bin_dir / ('web-server' + suffix)).resolve()
    assert cli.is_file() and server.is_file(), 'both binaries are required'
    reported = subprocess.check_output([str(cli), '--version'], text=True).strip()
    assert reported == f'luau-lifter V2 {version}', reported
    subprocess.run([str(cli), '--help'], check=True, stdout=subprocess.DEVNULL, timeout=30)
    fixture = ROOT / 'luau-lifter/tests/fixtures/cache_context.luaubc'
    output = subprocess.check_output([str(cli), str(fixture)], timeout=30)
    assert b'function Module.Read(' in output and b'return Module' in output, 'CLI fixture smoke test failed'
    smoke_server(server, fixture.read_bytes(), output)
    name = f'Tovek-V2-{version}-{args.platform}'
    args.out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='tovek-release-') as temp:
        package = pathlib.Path(temp) / name
        package.mkdir()
        for file in (cli, server, ROOT / 'LICENSE.txt', ROOT / 'README.md',
                     ROOT / 'release/QUICKSTART.txt', ROOT / 'decompile.client.luau',
                     ROOT / 'decompile-batch.client.luau'):
            shutil.copy2(file, package / file.name)
        if suffix:
            archive = args.out / (name + '.zip')
            with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=9) as stream:
                for file in sorted(package.iterdir()):
                    stream.write(file, name + '/' + file.name)
        else:
            archive = args.out / (name + '.tar.gz')
            with tarfile.open(archive, 'w:gz') as stream:
                stream.add(package, arcname=name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (args.out / f'SHA256SUMS-{args.platform}.txt').write_text(
        f'{digest}  {archive.name}\n', encoding='ascii', newline='\n')
    print(f'{reported}: smoke test passed; {archive.name} ({archive.stat().st_size:,} bytes)')


if __name__ == '__main__':
    main()
