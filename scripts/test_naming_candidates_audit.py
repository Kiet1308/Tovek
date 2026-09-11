import copy
import unittest

from naming_candidates_audit import validate


class NamingCandidatesAudit(unittest.TestCase):
    def report(self):
        return {'rows': [{'binding_id': 'b7'}], 'legacy_candidates': {
            'enabled': True, 'limits': {'bindings': 50000, 'candidates_per_binding': 24, 'name_bytes': 256},
            'rows': [{'binding_id': 'b7', 'final_binding_present': True, 'truncated': False,
                      'selected_hint': {'name': 'props', 'priority': 85}, 'candidates': [
                          {'name': 'props', 'priority': 85, 'reason': 'field',
                           'rule_site': {'file': 'ast/src/name_locals.rs', 'line': 10, 'column': 5}}]}]}}

    def test_rejects_wrong_identity_and_unsupported_winner(self):
        original = self.report()
        self.assertEqual(validate(original), [])
        for field, value, error in [('binding_id', 'b8', 'incorrect final identity mapping'),
                                    ('selected_hint', {'name': 'damage', 'priority': 85}, 'winner lacks candidate evidence')]:
            report = copy.deepcopy(original)
            report['legacy_candidates']['rows'][0][field] = value
            self.assertIn(error, validate(report))

    def test_unmapped_binding_and_truncated_winner_are_explicit(self):
        report = self.report()
        row = report['legacy_candidates']['rows'][0]
        report['rows'] = []
        row.update(final_binding_present=False, truncated=True, candidates=[])
        self.assertEqual(validate(report), [])


if __name__ == '__main__':
    unittest.main()
