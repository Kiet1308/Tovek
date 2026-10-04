#!/usr/bin/env python3
"""Recompute only the summary of a compact per-file bytecode baseline.

Per-file status/nonequiv/protos thresholds and provenance stay byte-for-byte
unchanged. Compact rows do not retain exact/equiv/differ/missing/extra splits;
the summary reports only reconstructible counts and never invents those splits.
"""
import argparse
import collections
import json
import pathlib


def summarize(document):
    rows = document.get('files')
    if not isinstance(rows, list) or not rows:
        raise ValueError('expected a nonempty compact baseline')
    seen = set()
    for row in rows:
        if (not isinstance(row, dict) or not isinstance(row.get('file'), str) or row['file'] in seen
                or not isinstance(row.get('status'), str)):
            raise ValueError('duplicate or invalid baseline row')
        seen.add(row['file'])
        for field in ('protos', 'nonequiv'):
            if type(row.get(field)) is not int or row[field] < 0:
                raise ValueError('invalid baseline threshold: ' + field)
    good = [row for row in rows if row['status'] == 'ok']
    return dict(model='compact-per-file-thresholds-v1', inputs=len(rows),
                status=dict(sorted(collections.Counter(row['status'] for row in rows).items())),
                original_protos=sum(row['protos'] for row in good),
                nonequiv=sum(row['nonequiv'] for row in good),
                files_fully_equiv=sum(row['nonequiv'] == 0 for row in good),
                contract='Counts are derived from immutable per-file thresholds. original_protos counts original prototypes; '
                         'nonequiv includes differing, missing and extra prototypes. Legacy exact/equiv/differ/missing/extra '
                         'splits and their ratio cannot be reconstructed from this compact baseline.')


def value_span(text, requested):
    """Locate one top-level JSON value without reserializing any baseline rows."""
    decoder = json.JSONDecoder()
    pos = 0
    def space(index):
        while index < len(text) and text[index].isspace():
            index += 1
        return index
    pos = space(pos)
    if text[pos:pos+1] != '{':
        raise ValueError('expected JSON object')
    pos += 1
    found, seen = None, set()
    while True:
        pos = space(pos)
        if text[pos:pos+1] == '}':
            break
        key, pos = decoder.raw_decode(text, pos)
        if not isinstance(key, str) or key in seen:
            raise ValueError('duplicate or invalid top-level JSON key')
        seen.add(key)
        pos = space(pos)
        if text[pos:pos+1] != ':':
            raise ValueError('expected JSON colon')
        pos = space(pos+1)
        start = pos
        _, pos = decoder.raw_decode(text, pos)
        if key == requested:
            found = start, pos
        pos = space(pos)
        if text[pos:pos+1] == ',':
            pos += 1
        elif text[pos:pos+1] != '}':
            raise ValueError('expected JSON comma')
    if found is None:
        raise ValueError('missing top-level ' + requested)
    return found


def refresh_bytes(data):
    text = data.decode('utf-8')
    document = json.loads(text)
    summary = summarize(document)
    if document.get('summary') == summary:
        return data, False
    start, end = value_span(text, 'summary')
    replacement = json.dumps(summary, indent=1, ensure_ascii=False).replace('\n', '\n ')
    updated = (text[:start] + replacement + text[end:]).encode('utf-8')
    # Verify the only semantic change is summary; surrounding raw text was spliced.
    check = json.loads(updated)
    original_without = {key: value for key, value in document.items() if key != 'summary'}
    if {key: value for key, value in check.items() if key != 'summary'} != original_without:
        raise ValueError('unexpected baseline mutation')
    return updated, True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('baselines', nargs='+', type=pathlib.Path)
    parser.add_argument('--write', action='store_true', help='replace only stale summary values; default is check-only')
    args = parser.parse_args()
    stale = 0
    for path in args.baselines:
        updated, changed = refresh_bytes(path.read_bytes())
        stale += changed
        if changed and args.write:
            path.write_bytes(updated)
        print(f'{path}: ' + ('updated summary' if changed and args.write else 'stale summary' if changed else 'summary matches rows'))
    return int(stale > 0 and not args.write)


if __name__ == '__main__':
    raise SystemExit(main())
