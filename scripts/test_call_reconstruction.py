import copy
import unittest

from call_reconstruction import MODEL, occurrences_at, validate, validate_parser_calls


def fixture():
    source = '-- 界\nlocal f = helper\nf()\nf()'.encode('utf-8')
    def position(offset):
        prefix = source[:offset]
        return dict(byte_offset=offset, line_one_based=prefix.count(b'\n') + 1,
                    column_one_based=len(prefix.rsplit(b'\n', 1)[-1].decode('utf-8')) + 1)
    def span(start, end): return dict(start=position(start), end=position(end))
    def location(start, end):
        a, b = position(start), position(end)
        return f"{a['line_one_based']-1},{a['column_one_based']-1} - {b['line_one_based']-1},{b['column_one_based']-1}"
    offsets = [source.index(b'f()'), source.rindex(b'f()')]
    trace = dict(functions=[dict(prototype=3)], final_bindings=[dict(binding_id='b1'), dict(binding_id='b2')],
        call_reconstruction=dict(schema_version=2, model=MODEL, event_limit=4096, occurrence_limit=100000,
            callees_limit=50000, omitted_events=0, omitted_occurrences=0, omitted_callee_registrations=0,
            events=[dict(event_id=1, producer='statement_deinline', callee_binding_at_creation='b9', callee_prototype=3, evidence='equivalent_call_inference')],
            occurrences=[dict(event_id=1, current_callee_binding='b1', span=span(offset, offset + 3)) for offset in offsets]),
        output_map=dict(bindings=[dict(binding_id='b1', span=span(offset, offset + 1)) for offset in offsets]))
    tree = [dict(type='AstExprCall', location=location(offset, offset + 3),
                 func=dict(type='AstExprLocal', location=location(offset, offset + 1))) for offset in offsets]
    return trace, source, tree


class CallReconstructionTests(unittest.TestCase):
    def test_inference_cannot_be_promoted_and_historical_events_stay_unclassified(self):
        trace, source, _ = fixture()
        trace['call_reconstruction']['events'][0]['evidence'] = 'original_call_proved'
        self.assertTrue(validate(trace, source))
        trace['call_reconstruction']['schema_version'] = 1
        trace['call_reconstruction']['model'] = 'committed-call-reconstruction-events-v1'
        del trace['call_reconstruction']['events'][0]['evidence']
        self.assertEqual(validate(trace, source), [])

    def test_hint_pc_ranges_are_separate_from_original_callsite_evidence(self):
        trace, source, _ = fixture()
        trace['functions'] = [dict(prototype=3, instruction_count=12), dict(prototype=4, instruction_count=5)]
        hints = dict(pc_limit=200000, region_limit=8192, truncated=False,
            regions=[dict(caller_prototype=4, helper_prototype=3, start_pc=1, end_pc_exclusive=4)])
        trace['call_reconstruction']['search_hints'] = hints
        self.assertEqual(validate(trace, source), [])
        self.assertEqual(occurrences_at(trace, source.rindex(b'f()'))[0]['input_callsite'], 'unknown')
        hints['regions'][0]['end_pc_exclusive'] = 6
        self.assertTrue(validate(trace, source))

    def test_clone_occurrences_and_unknown_caller_origin(self):
        trace, source, tree = fixture()
        self.assertEqual(validate(trace, source), [])
        self.assertEqual(validate_parser_calls(trace, source, tree), [])
        hit = occurrences_at(trace, source.rindex(b'f()'))
        self.assertEqual(hit[0]['creation_event']['callee_prototype'], 3)
        self.assertEqual(hit[0]['input_callsite'], 'unknown')
        self.assertEqual(occurrences_at(trace, len(source)), [])

    def test_missing_occurrence_does_not_fabricate_origin(self):
        trace, source, tree = fixture()
        trace['call_reconstruction']['occurrences'] = []
        self.assertEqual(validate(trace, source), [])
        self.assertEqual(validate_parser_calls(trace, source, tree), [])
        self.assertEqual(occurrences_at(trace, 1), [])
        self.assertEqual(validate({}), [])

    def test_corrupt_evidence_is_rejected(self):
        trace, source, _ = fixture()
        mutations = {
            'duplicate_event': lambda r: r['events'].append(copy.deepcopy(r['events'][0])),
            'dangling_event': lambda r: r['occurrences'][0].update(event_id=2),
            'invented_caller_pc': lambda r: r['events'][0].update(caller_pc=7),
            'synth_claims_prototype': lambda r: r['events'][0].update(producer='terminal_synthesis'),
            'unknown_prototype': lambda r: r['events'][0].update(callee_prototype=999),
            'creation_id_overflow': lambda r: r['events'][0].update(callee_binding_at_creation='b18446744073709551616'),
            'unknown_current_binding': lambda r: r['occurrences'][0].update(current_callee_binding='b999'),
            'false_omission': lambda r: r.update(omitted_events=1),
            'duplicate_span': lambda r: r['occurrences'].append(copy.deepcopy(r['occurrences'][0])),
            'reversed_order': lambda r: r['occurrences'].reverse(),
            'clipped_utf8': lambda r: r['occurrences'][0]['span']['start'].update(byte_offset=4),
            'wrong_column': lambda r: r['occurrences'][0]['span']['start'].update(column_one_based=2),
            'oversize_events': lambda r: r.update(events=r['events'] * 4097),
            'unknown_schema': lambda r: r.update(model='semantic-certificate'),
            'boolean_id': lambda r: r['events'][0].update(event_id=True),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                changed = copy.deepcopy(trace)
                mutate(changed['call_reconstruction'])
                self.assertTrue(validate(changed, source))

    def test_parser_rejects_clipped_call_and_wrong_callee_even_if_ids_exist(self):
        trace, source, tree = fixture()
        trace['call_reconstruction']['occurrences'][0]['current_callee_binding'] = 'b2'
        self.assertEqual(validate(trace, source), [])
        self.assertTrue(validate_parser_calls(trace, source, tree))
        trace, source, tree = fixture()
        end = trace['call_reconstruction']['occurrences'][0]['span']['end']
        end['byte_offset'] -= 1
        end['column_one_based'] -= 1
        self.assertEqual(validate(trace, source), [])
        self.assertTrue(validate_parser_calls(trace, source, tree))


if __name__ == '__main__':
    unittest.main()
