import unittest
from output_quality import analyze_tree, compare_rows


def local(name, location):
    return {'type': 'AstLocal', 'name': name, 'location': location}


def read(binding):
    return {'type': 'AstExprLocal', 'local': binding}


def block(*body):
    return {'type': 'AstStatBlock', 'body': list(body)}


def declaration(binding):
    return {'type': 'AstStatLocal', 'vars': [binding], 'values': [{'type': 'AstExprBinary'}]}


class OutputQualityTests(unittest.TestCase):
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
