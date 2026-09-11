#!/usr/bin/env python3
"""Verify per-process RSS sampling against a separate 64 MiB resident allocation."""
import argparse
import json
import pathlib
import subprocess
import sys
import time

from benchmark_v2 import peak_rss_reader


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=pathlib.Path, required=True)
    args = parser.parse_args()
    rows = []
    code = ('import sys,time; n=int(sys.argv[1]); data=bytearray(n); '
            'data[::4096]=b"x"*len(data[::4096]); print("ready",flush=True); time.sleep(.3)')
    for size in (0, 64 * 1024 * 1024):
        process = subprocess.Popen([sys.executable, '-I', '-c', code, str(size)], stdout=subprocess.PIPE)
        if process.stdout.readline().strip() != b'ready': raise ValueError('allocation helper failed')
        reader, peak = peak_rss_reader(process), None
        while process.poll() is None:
            value = reader()
            if value is not None: peak = max(peak or 0, value)
            time.sleep(.01)
        if process.returncode != 0 or peak is None: raise ValueError('RSS measurement unavailable or child failed')
        rows.append(dict(resident_allocation_bytes=size, sampled_peak_rss_bytes=peak))
    growth = rows[1]['sampled_peak_rss_bytes'] - rows[0]['sampled_peak_rss_bytes']
    if not 48 * 1024 * 1024 <= growth <= 80 * 1024 * 1024:
        raise ValueError('per-process resident RSS growth has wrong units or scope')
    report = dict(schema_version=1, status='passed', platform=sys.platform, rows=rows, growth_bytes=growth,
                  contract='Separate child processes touch every page of a 64 MiB buffer. The sampled high-water RSS must grow by 48–80 MiB; this checks units and per-process scope, not exact allocator overhead or the last unsampled instant.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
