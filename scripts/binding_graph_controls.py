#!/usr/bin/env python3
"""Check scope edge cases against the real pinned parser, with explicit expectations."""
import argparse
import json
import pathlib

from binding_graph import LIMITS, Refused, digest, lexical_graph, parse_source


# Expected rows: name, declaration kind, token count, lexical capture, direct write.
CASES = [
    ('shadow', 'local x = 1\ndo local x = x; print(x) end\nreturn x',
     [('x', 'local', 3, False, False), ('x', 'local', 2, False, False)]),
    ('for_header_outer_scope', 'local i = 3\nfor i = i, i do print(i) end\nfor i, value in next, {i} do print(i, value) end\nreturn i',
     [('i', 'local', 5, False, False), ('i', 'iteration', 2, False, False),
      ('i', 'iteration', 2, False, False), ('value', 'iteration', 2, False, False)]),
    ('repeat_until_inner_scope', 'local x = 4\nrepeat local x = x - 1 until x == 0\nreturn x',
     [('x', 'local', 3, False, False), ('x', 'local', 2, False, False)]),
    ('recursive_capture', 'local function f(x) return function() x += 1; return f(x) end end\nreturn f',
     [('f', 'local_function', 3, True, False), ('x', 'parameter', 3, True, True)]),
    ('colon_capture', 'local t = {}\nfunction t:m(x) return function() return self, x end end\nreturn t',
     [('t', 'local', 3, False, False), ('self', 'implicit_self', 1, True, False), ('x', 'parameter', 2, True, False)]),
    ('initializer_outer_scope', 'local x = 2\nlocal x = function() return x end\nreturn x',
     [('x', 'local', 2, True, False), ('x', 'local', 2, False, False)]),
    ('interpolation', 'local x = 2\nreturn `hello {x}`', [('x', 'local', 2, False, False)]),
    ('utf8_crlf', '-- 界\r\nlocal x = "界"; x = x .. "!"\r\nreturn x', [('x', 'local', 4, False, True)]),
    ('typeof_is_not_runtime_capture', 'local x = 1\nlocal function f() type T = typeof(x); return nil end\nreturn f',
     [('x', 'local', 2, False, False), ('f', 'local_function', 2, False, False)]),
    ('shadow_implicit_self', 'local self = 1\nlocal t = {}\nfunction t:m() local self = self; return self end\nreturn self',
     [('self', 'local', 2, False, False), ('t', 'local', 2, False, False),
      ('self', 'implicit_self', 1, False, False), ('self', 'local', 2, False, False)]),
    ('index_stores_read_base_and_key', 'local t = {}; local k = 1; t[k] = 2; t[k] += 3; return t',
     [('t', 'local', 4, False, False), ('k', 'local', 3, False, False)]),
    ('annotation_typeof_references', 'local x = 1\nlocal f: (typeof(x)) -> typeof(x) = function(y) return y end\nreturn f',
     [('x', 'local', 3, False, False), ('f', 'local', 2, False, False), ('y', 'parameter', 2, False, False)]),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ast', type=pathlib.Path, required=True)
    parser.add_argument('--report', type=pathlib.Path, required=True)
    args = parser.parse_args()
    rows = []
    for name, text, expected in CASES:
        source = text.encode('utf-8')
        graph = lexical_graph(parse_source(args.ast, source), source)
        actual = [(r['name'], r['kind'], len(r['tokens']), r['captured_in_output'], r['written_in_output'])
                  for r in graph['declarations']]
        again = lexical_graph(parse_source(args.ast, source), source)
        rows.append(dict(case=name, source=text, expected=expected, actual=actual, graph=graph,
                         status='passed' if actual == expected and graph == again else 'failed'))
    for name, source, limits, expected in [
        ('parser_stdout_cap', b'local x = 1; return x', LIMITS | dict(json_bytes=10), 'parser_output_budget'),
        ('parser_stderr_cap', b'local =', LIMITS | dict(stderr_bytes=1), 'parser_output_budget'),
        ('parser_timeout', b'local x = 1; return x', LIMITS | dict(parser_seconds=0), 'parser_time_budget'),
    ]:
        try:
            parse_source(args.ast, source, limits)
            actual = 'unexpected_success'
        except Refused as error:
            actual = str(error)
        rows.append(dict(case=name, expected=expected, actual=actual,
                         status='passed' if actual == expected else 'failed'))
    report = dict(schema_version=1, ast_sha256=digest(args.ast.read_bytes()),
                  summary=dict(controls=len(rows), passed=sum(r['status'] == 'passed' for r in rows)), rows=rows)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report['summary']))
    return int(any(row['status'] != 'passed' for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
