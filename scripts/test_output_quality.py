import unittest
from output_quality import analyze_tree, compare_rows, readable_conditionals


def local(name, location):
    return {'type': 'AstLocal', 'name': name, 'location': location}


def read(binding):
    return {'type': 'AstExprLocal', 'local': binding}


def block(*body):
    return {'type': 'AstStatBlock', 'body': list(body)}


def declaration(binding):
    return {'type': 'AstStatLocal', 'vars': [binding], 'values': [{'type': 'AstExprBinary'}]}


class OutputQualityTests(unittest.TestCase):
    def test_default_conditional_gate_keeps_value_arity_and_width_limits(self):
        yes = {'type': 'AstExprConstantBool', 'value': False}
        no = {'type': 'AstExprConstantNil'}
        select = {'type': 'AstExprIfElse', 'location': '0,7 - 0,39',
                  'condition': {'type': 'AstExprGlobal', 'global': 'flag'},
                  'trueExpr': yes, 'falseExpr': no}
        tree = block({'type': 'AstStatReturn', 'list': [select]})
        text = 'return if flag then false else nil'
        self.assertTrue(readable_conditionals(tree, text))
        self.assertFalse(readable_conditionals(tree, ' ' * 50 + text))
        self.assertFalse(readable_conditionals(block({'type': 'AstStatIf', 'condition': select}), text))
        select['falseExpr'] = {'type': 'AstExprCall'}
        self.assertFalse(readable_conditionals(tree, text))
        select['falseExpr'] = dict(select)
        self.assertFalse(readable_conditionals(tree, text))

    def test_default_conditional_gate_accepts_only_literal_numeric_spellings(self):
        number = lambda value: {'type': 'AstExprConstantNumber', 'value': value}
        integer = lambda value: {'type': 'AstExprConstantInteger', 'value': value}
        minus = lambda value: {'type': 'AstExprUnary', 'op': 'Minus', 'expr': value}
        group = lambda value: {'type': 'AstExprGroup', 'expr': value}
        binary = lambda op, left, right: {'type': 'AstExprBinary', 'op': op, 'left': left, 'right': right}
        nan = group(binary('Div', number(0), number(0)))
        call = {'type': 'AstExprCall'}
        name = {'type': 'AstExprGlobal', 'global': 'value'}
        accepted = [number(1), minus(number(1)), minus(number(0)), number(float('inf')),
                    minus(number(float('inf'))), nan, minus(nan), integer(1), minus(integer(1)),
                    integer(-(2 ** 63))]
        rejected = [binary('Add', number(1), number(2)), binary('Div', number(1), number(0)),
                    binary('Div', integer(0), integer(0)), binary('Div', call, number(0)),
                    minus(name), minus(minus(number(1))), group(call), group(binary('Mul', number(0), number(0)))]
        for expected, values in [(True, accepted), (False, rejected)]:
            for value in values:
                for arm in [group(value), {'type': 'AstExprIndexExpr', 'expr': name, 'index': group(value)}]:
                    with self.subTest(arm=arm):
                        select = {'type': 'AstExprIfElse', 'location': '0,7 - 0,40',
                                  'condition': name, 'trueExpr': arm, 'falseExpr': number(2)}
                        tree = block({'type': 'AstStatReturn', 'list': [select]})
                        self.assertEqual(readable_conditionals(tree, 'return if flag then value else 2'), expected)
        for key in [name, read(local('value', '0,0'))]:
            select['trueExpr'] = {'type': 'AstExprIndexExpr', 'expr': name, 'index': key}
            self.assertFalse(readable_conditionals(tree, 'return if flag then value else 2'))

    def test_compact_annotations_do_not_hide_inference_from_quality_counts(self):
        full = analyze_tree(block(), 'f() -- equivalent call inferred; original call site unknown\n')
        compact = analyze_tree(block(), 'f() -- inferred call\n-- inferred helper\n')
        self.assertEqual(full['inferred_call_annotations'], 1)
        self.assertEqual(compact['inferred_call_annotations'], 1)
        self.assertEqual(compact['compact_inferred_call_annotations'], 1)
        self.assertEqual(compact['full_inferred_call_annotations'], 0)
        site = analyze_tree(block(), 'if p and f() then -- inferred equivalent call\n\tg() -- inferred equivalent call\nend\n')
        self.assertEqual(site['full_inferred_call_annotations'], 2)
        self.assertEqual(site['inferred_call_annotations'], 2)

    def test_shadowed_names_and_captured_sole_use_are_separate(self):
        outer, inner = local('v', '1,0'), local('v', '4,0')
        tree = block(declaration(outer), {'type': 'AstExprFunction', 'body': block(
            declaration(inner), read(inner), read(outer))})
        metrics = analyze_tree(tree)
        self.assertEqual(metrics['generated_single_use_AstExprBinary'], 2)
        self.assertEqual(metrics['captured_single_use_AstExprBinary'], 1)
        self.assertEqual(metrics['uncaptured_generated_single_use_AstExprBinary'], 1)

    def test_repeated_use_is_not_a_single_use_candidate(self):
        value = local('v', '1,0')
        metrics = analyze_tree(block(declaration(value), read(value), read(value)))
        self.assertNotIn('single_use_AstExprBinary', metrics)

    def test_per_file_regression_survives_aggregate_improvement_and_missing_files(self):
        def row(path, count):
            return {'path': path, 'status': 'passed', 'metrics': {'temps': count}}
        result = compare_rows([row('a', 10), row('b', 0), row('c', 2)],
                              [row('a', 0), row('b', 1)], ['temps'])
        self.assertEqual(result['gate_failures'], [
            {'path': 'b', 'metric': 'temps', 'delta': 1},
            {'path': 'c', 'reason': 'missing_or_unparsed_current'}])
        self.assertEqual(result['rows'][-1]['status'], 'unmeasured')


if __name__ == '__main__':
    unittest.main()
