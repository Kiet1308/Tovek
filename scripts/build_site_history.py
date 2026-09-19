"""Regenerate the release article's pinned public commit inventory (no private data)."""

import html
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASE = '27422b9f8e0aeca0e61db55992930f17a98f4099'
HEAD = '465c9dceff1f1d0c925edcc77dec8afaa1baeb37'


def main():
    log = subprocess.check_output(
        ['git', 'log', f'{BASE}..{HEAD}', '--format=%H%x09%ad%x09%s', '--date=short'],
        cwd=ROOT, text=True, encoding='utf-8',
    )
    commits = []
    for line in log.splitlines():
        commit, date, title = line.split('\t', 2)
        commits.append(dict(hash=commit, short=commit[:7], date=date, title=title))
    assert len(commits) == 130, 'The article describes a fixed, reviewed release range.'
    inventory = dict(base=BASE, head=HEAD, count=len(commits), commits=commits)
    (ROOT / 'docs/assets/v2-history.json').write_bytes(
        (json.dumps(inventory, ensure_ascii=False, indent=2) + '\n').encode('utf-8')
    )
    rows = []
    for item in commits:
        rows.append(
            f'          <li data-hash="{item["hash"]}"><a href="https://github.com/Kiet1308/Tovek/commit/{item["hash"]}">'
            f'<time datetime="{item["date"]}">{item["date"]}</time>'
            f'<span class="commit-title">{html.escape(item["title"])}<small>{item["short"]} ↗</small></span></a></li>'
        )
    path = ROOT / 'docs/changelog.html'
    content = path.read_text(encoding='utf-8')
    before, remainder = content.split('          <!-- HISTORY_START -->', 1)
    _, after = remainder.split('          <!-- HISTORY_END -->', 1)
    content = before + '          <!-- HISTORY_START -->\n' + '\n'.join(rows) + '\n          <!-- HISTORY_END -->' + after
    path.write_bytes(content.encode('utf-8'))
    print(f'Generated {len(commits)} commit links: {BASE[:7]}..{HEAD[:7]}')


if __name__ == '__main__':
    main()
