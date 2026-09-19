"""Explicit, rate-limited providers for the public decompiler benchmark."""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
import urllib.error
import urllib.request

ENDPOINT = 'https://api.lua.expert/decompile'
MAX_RESPONSE = 16 * 1024 * 1024


def digest(data):
    return hashlib.sha256(data).hexdigest()


def classify_body(body):
    if re.search(rb'(?m)^Bytecode version \(\d+\) unhandled\s*$', body):
        return 'unsupported_version'
    return 'output' if body.strip() else 'empty_response'


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise urllib.error.HTTPError(req.full_url, code, 'redirect refused', headers, fp)


class Expert:
    def __init__(self, requests_per_minute=120, timeout=30):
        if not 1 <= requests_per_minute <= 240:
            raise ValueError('benchmark rate must be 1..240 requests/minute')
        self.interval = 60 / requests_per_minute
        self.timeout = timeout
        self.next_request = 0
        self.opener = urllib.request.build_opener(NoRedirect)
        self.identity = dict(kind='hosted_api', endpoint=ENDPOINT, backend_version='not exposed',
                             response_contract='plain text; HTTP 200 is not a success oracle',
                             timing='HTTPS request wall time including network/TLS, excluding rate-limit wait')

    def invoke(self, bytecode, retry=True):
        payload = json.dumps({'script': base64.b64encode(bytecode.read_bytes()).decode('ascii')}).encode()
        attempts, body = [], b''
        for number in range(3 if retry else 1):
            time.sleep(max(0, self.next_request - time.monotonic()))
            start = time.perf_counter()
            self.next_request = time.monotonic() + self.interval
            request = urllib.request.Request(ENDPOINT, data=payload, headers={
                'Content-Type': 'application/json', 'User-Agent': 'Tovek-public-benchmark/1.0'})
            status, headers, error = None, {}, None
            try:
                with self.opener.open(request, timeout=self.timeout) as response:
                    status, headers = response.status, dict(response.headers)
                    body = response.read(MAX_RESPONSE + 1)
                outcome = classify_body(body) if status == 200 else 'http_error'
                if len(body) > MAX_RESPONSE:
                    outcome, body = 'response_limit', body[:MAX_RESPONSE]
            except urllib.error.HTTPError as exc:
                with exc:
                    status, headers, body = exc.code, dict(exc.headers), exc.read(MAX_RESPONSE)
                outcome, error = 'http_error', str(exc)
            except (OSError, TimeoutError, urllib.error.URLError) as exc:
                outcome, error, body = 'transport_error', str(exc), b''
            attempts.append(dict(status=outcome, http_status=status, seconds=time.perf_counter() - start,
                                 error=error, utc=time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
                                 headers={k.lower(): v for k, v in headers.items()
                                          if k.lower() in ('date', 'content-type', 'retry-after', 'server', 'cf-ray')}))
            if not retry or not (status in (429, 502, 503, 504) or outcome == 'transport_error'):
                break
            if number < 2:
                retry_after = next((v for k, v in headers.items() if k.lower() == 'retry-after'), '2')
                try: delay = min(60, max(2 ** (number + 1), float(retry_after)))
                except ValueError: delay = 10
                time.sleep(delay)
        return dict(status=outcome, attempts=attempts, output_sha256=digest(body)), body


class Native:
    def __init__(self, binary, timeout=30):
        self.binary, self.timeout = Path(binary).resolve(strict=True), timeout
        self.identity = dict(kind='local_cli', binary=str(self.binary), sha256=digest(self.binary.read_bytes()),
                             arguments=['<identical raw bytecode>'], threads=1,
                             timing='process wall time including startup and I/O; warm filesystem cache')

    def invoke(self, bytecode, retry=True):
        env = {k: v for k, v in os.environ.items() if not k.upper().startswith('MEDAL_')}
        env['RAYON_NUM_THREADS'] = '1'
        start = time.perf_counter()
        try:
            result = subprocess.run([str(self.binary), str(bytecode.resolve())], capture_output=True,
                                    timeout=self.timeout, env=env)
            body, error = result.stdout, result.stderr.decode('utf-8', errors='replace')
            status = classify_body(body) if result.returncode == 0 else 'process_error'
            if re.search(r'(unsupported.*(?:bytecode|version)|(?:bytecode|version).*unsupported)', error, re.I):
                status = 'unsupported_version'
            code = result.returncode
        except subprocess.TimeoutExpired:
            body, error, status, code = b'', 'process timeout', 'timeout', None
        return dict(status=status, attempts=[dict(status=status, exit_code=code,
                    seconds=time.perf_counter() - start, error=error[:4000])], output_sha256=digest(body)), body
