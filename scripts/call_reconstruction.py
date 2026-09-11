"""Validate producer-event diagnostics separately from input semantic proofs."""
import bisect
import re

KINDS = {'statement_deinline', 'expression_deinline', 'arithmetic_deinline', 'terminal_synthesis'}
MODEL = 'committed-call-reconstruction-events-v1'


def compact_annotation(text):
    labels = {
        ' [-O2 INLINED, UNHOOKABLE] reconstructed definition;': 'inferred helper',
        'inlined by Luau -O2 (UNHOOKABLE)': 'inferred call',
        ' [-O2 INLINED, UNHOOKABLE] reconstructed call': 'inferred call',
        ' equivalent arithmetic calls inferred from this bytecode helper; original call sites unknown': 'inferred arithmetic helper',
        'equivalent fixed-count loop synthesized; original loop unknown': 'synthesized arithmetic loop',
    }
    return 'synthesized helper' if text.startswith('[DEDUP] synthesized from ') else labels.get(text)


def validate(trace, source=None):
    report = trace.get('call_reconstruction')
    if report is None:
        return []  # Historical traces remain unclassified.
    errors = []
    def require(ok, reason):
        if not ok and len(errors) < 20: errors.append('call reconstruction: ' + reason)
    def integer(value): return type(value) is int and value >= 0
    def binding(value):
        return isinstance(value, str) and re.fullmatch(r'b(0|[1-9][0-9]*)', value) and int(value[1:]) < 2**64
    try:
        require(report['schema_version'] == 1 and report['model'] == MODEL, 'unknown schema/model')
        require((report['event_limit'], report['occurrence_limit'], report['callees_limit']) == (4096, 100000, 50000), 'limits differ')
        events, occurrences = report['events'], report['occurrences']
        require(isinstance(events, list) and len(events) <= 4096, 'event budget exceeded')
        require(isinstance(occurrences, list) and len(occurrences) <= 100000, 'occurrence budget exceeded')
        if errors: return errors
        for field in ('omitted_events', 'omitted_occurrences', 'omitted_callee_registrations'):
            require(integer(report[field]), 'invalid omission count')
        require(not report['omitted_events'] or len(events) == 4096, 'omission before event cap')
        require(not report['omitted_occurrences'] or len(occurrences) == 100000, 'omission before occurrence cap')
        prototypes = {f['prototype'] for f in trace['functions']}
        final = {b['binding_id'] for b in trace['final_bindings']}
        for index, event in enumerate(events):
            require(set(event) == {'event_id', 'producer', 'callee_binding_at_creation', 'callee_prototype'}, 'unknown event evidence fields')
            require(type(event['event_id']) is int and event['event_id'] == index + 1, 'event IDs are not unique creation order')
            require(event['producer'] in KINDS, 'unknown producer')
            require(binding(event['callee_binding_at_creation']), 'invalid creation binding ID')
            proto = event['callee_prototype']
            require(proto is None or integer(proto) and proto in prototypes, 'callee prototype absent from input trace')
            require(event['producer'] != 'terminal_synthesis' or proto is None, 'synthesized helper claims input prototype')
        event_ids = {event['event_id'] for event in events}
        seen, order = set(), []
        line_starts = [0] + [index + 1 for index, byte in enumerate(source) if byte == 10] if source is not None else []
        for occurrence in occurrences:
            require(set(occurrence) == {'event_id', 'span', 'current_callee_binding'}, 'unknown occurrence evidence fields')
            require(type(occurrence['event_id']) is int and occurrence['event_id'] in event_ids, 'occurrence has no creation event')
            callee = occurrence['current_callee_binding']
            require(callee is None or callee in final, 'current callee is not a final binding')
            span = occurrence['span']
            start, end = span['start']['byte_offset'], span['end']['byte_offset']
            require(integer(start) and integer(end) and start < end, 'invalid call span')
            require((start, end) not in seen, 'call span repeated')
            seen.add((start, end)); order.append((start, end, occurrence['event_id']))
            if source is not None:
                require(end <= len(source), 'call span outside source')
                for position in (span['start'], span['end']):
                    offset = position['byte_offset']
                    line = bisect.bisect_right(line_starts, offset)
                    column = len(source[line_starts[line - 1]:offset].decode('utf-8')) + 1
                    require(position['line_one_based'] == line and position['column_one_based'] == column, 'call position coordinates differ')
        require(order == sorted(order), 'call occurrences unsorted')
    except (KeyError, TypeError, ValueError, IndexError, UnicodeError) as error:
        require(False, 'malformed report: ' + type(error).__name__)
    return errors


def occurrences_at(trace, offset):
    report = trace.get('call_reconstruction')
    if report is None: return []
    events = {row['event_id']: row for row in report['events']}
    return [dict(occurrence=row, creation_event=events[row['event_id']], input_callsite='unknown')
            for row in report['occurrences']
            if row['span']['start']['byte_offset'] <= offset < row['span']['end']['byte_offset']]


def validate_parser_calls(trace, source, tree):
    report = trace.get('call_reconstruction')
    if report is None or not report['occurrences']: return []
    starts = [0] + [i + 1 for i, byte in enumerate(source) if byte == 10]
    def bounds(location):
        a, b, c, d = (int(n) for n in re.findall(r'\d+', location))
        return starts[a] + b, starts[c] + d
    calls = {}
    def walk(node):
        if isinstance(node, dict):
            if node.get('type') == 'AstExprCall':
                calls[bounds(node['location'])] = node
            for child in node.values(): walk(child)
        elif isinstance(node, list):
            for child in node: walk(child)
    walk(tree)
    tokens = {(t['span']['start']['byte_offset'], t['span']['end']['byte_offset']): t['binding_id']
              for t in trace['output_map']['bindings']}
    errors = []
    for row in report['occurrences']:
        extent = row['span']['start']['byte_offset'], row['span']['end']['byte_offset']
        node = calls.get(extent)
        if node is None:
            errors.append('reconstructed call span is not an exact parser call')
        elif row['current_callee_binding'] is not None:
            func = node['func']
            if func['type'] != 'AstExprLocal' or tokens.get(bounds(func['location'])) != row['current_callee_binding']:
                errors.append('reconstructed callee disagrees with parser binding')
    return errors[:20]
