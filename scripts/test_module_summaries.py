import unittest

from module_summaries import components, static_path, summarize


def glob(name):
    return {'type': 'AstExprGlobal', 'global': name}


def local(name, location='0,0 - 0,1'):
    return dict(type='AstLocal', name=name, location=location)


def read(name):
    return dict(type='AstExprLocal', local=local(name))


def index(base, key):
    return dict(type='AstExprIndexName', expr=base, index=key, op='.')


def call(func, args=None):
    return dict(type='AstExprCall', func=func, args=args or [], location='0,0 - 1,0', self=False)


def ret(*values):
    return dict(type='AstStatReturn', list=list(values))


def function(*statements):
    return dict(type='AstExprFunction', args=[], vararg=False, location='1,0 - 3,0',
                body=dict(type='AstStatBlock', body=list(statements)))


def module(other=None, forward=False):
    body = []
    if other:
        require = call(glob('require'), [index(index(glob('script'), 'Parent'), other)])
        body.append(dict(type='AstStatLocal', vars=[local('Other')], values=[require]))
    value = call(read('Other')) if forward else dict(type='AstExprConstantNumber', value=7)
    body.append(ret(function(ret(value))))
    return dict(type='AstStatBlock', body=body)


def entry(name):
    return dict(id=name, script_path=['Package', name])


class ModuleSummaries(unittest.TestCase):
    def test_static_sibling_path_and_dynamic_refusal(self):
        self.assertEqual(static_path(index(index(glob('script'), 'Parent'), 'Other'), ['Package', 'A']), ('Package', 'Other'))
        self.assertIsNone(static_path(call(glob('lookup')), ['Package', 'A']))
        self.assertIsNone(static_path(index(glob('script'), '../Other'), ['A']))

    def test_forwarded_results_keep_origin_and_cycles_stay_unknown(self):
        entries = [entry('A'), entry('B')]
        result = summarize(entries, {'A': module('B', True), 'B': module()})
        self.assertEqual(result['summary']['resolved_calls'], 1)
        self.assertEqual(result['rows'][0]['functions'][0]['returns'][0]['origin_function'], 'B:f0')
        result = summarize(entries, {'A': module('B', True), 'B': module('A', True)})
        self.assertEqual(result['summary']['unknown_returns'], 2)
        self.assertEqual(result['sccs'], [dict(modules=['A', 'B'], cyclic=True)])

    def test_duplicate_paths_and_rebound_imports_do_not_resolve_calls(self):
        entries = [entry('A'), entry('B'), dict(id='B2', script_path=['Package', 'B'])]
        result = summarize(entries, {'A': module('B', True), 'B': module(), 'B2': module()})
        self.assertEqual(result['summary']['require_status'], {'ambiguous_path': 1})
        self.assertEqual(result['summary']['resolved_calls'], 0)
        for target in [read('Other'), glob('require')]:
            tree = module('B', True)
            tree['body'].insert(1, dict(type='AstStatAssign', vars=[target], values=[glob('custom')]))
            result = summarize(entries[:2], {'A': tree, 'B': module()})
            self.assertEqual(result['summary']['resolved_calls'], 0)

    def test_export_table_escape_and_duplicate_keys_refuse(self):
        key = dict(type='AstExprConstantString', value='run')
        item = dict(key=key, value=function(ret(dict(type='AstExprConstantNil'))))
        table = dict(type='AstExprTable', items=[item])
        tree = dict(type='AstStatBlock', body=[dict(type='AstStatLocal', vars=[local('exports')], values=[table]), ret(read('exports'))])
        result = summarize([entry('A')], {'A': tree})
        self.assertEqual(result['rows'][0]['export_status'], 'private_literal_exports')
        tree['body'].insert(1, dict(type='AstStatExpr', expr=call(glob('observe'), [read('exports')])))
        result = summarize([entry('A')], {'A': tree})
        self.assertEqual(result['rows'][0]['export_status'], 'export_table_observed_or_mutated')
        tree['body'].pop(1)
        table['items'].append(item)
        result = summarize([entry('A')], {'A': tree})
        self.assertEqual(result['rows'][0]['export_status'], 'dynamic_or_duplicate_export_key')

    def test_nested_calls_are_owned_by_their_own_function(self):
        inner = function(ret(call(glob('unknown'))))
        tree = dict(type='AstStatBlock', body=[ret(function(ret(inner)))])
        result = summarize([entry('A')], {'A': tree})
        self.assertEqual(len(result['rows'][0]['calls']), 1)
        self.assertEqual(result['rows'][0]['calls'][0]['function'], 'A:f1')

    def test_argument_roles_require_a_fixed_exact_pack(self):
        target = module()
        target['body'][0]['list'][0]['args'] = [local('input')]
        for open_pack in [False, True]:
            caller = module('B', True)
            fn = caller['body'][-1]['list'][0]
            fn['args'] = [local('value')]
            fn['body']['body'][0]['list'][0]['args'] = [call(glob('unknown')) if open_pack else read('value')]
            report = summarize([entry('A'), entry('B')], {'A': caller, 'B': target})
            outer = report['rows'][0]['calls'][0]
            self.assertEqual(outer['arity_status'], 'unknown' if open_pack else 'fixed_exact')
            self.assertEqual(len(outer['argument_roles']), 0 if open_pack else 1)
            if not open_pack:
                self.assertEqual(outer['argument_roles'][0]['parameter']['observed_name'], 'input')

    def test_module_alias_cycles_do_not_pick_an_export(self):
        trees = {}
        for name, other in [('A', 'B'), ('B', 'A')]:
            trees[name] = dict(type='AstStatBlock', body=[ret(call(glob('require'), [index(index(glob('script'), 'Parent'), other)]))])
        report = summarize([entry('A'), entry('B')], trees)
        self.assertEqual(report['sccs'], [dict(modules=['A', 'B'], cyclic=True)])
        self.assertEqual(report['rows'][0]['exports']['default'], dict(module='B', export='default'))
        self.assertEqual(report['summary']['resolved_calls'], 0)

    def test_large_scc_is_iterative_and_stable(self):
        graph = {str(i): {str((i + 1) % 1500)} for i in range(1500)}
        self.assertEqual(components(graph), [sorted(graph)])


if __name__ == '__main__':
    unittest.main()
