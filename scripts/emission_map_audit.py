"""Check final identifier spans against source bytes and recorded binding IDs.

These checks validate location/identity consistency, not value provenance or
instruction equivalence. Existing SSA trace validation checks the ancestry
links followed from a final binding to its original statement PC sets.
"""
import bisect
import re


def parser_occurrences(root, source):
    """Use pinned AST declaration identity, not identifier spelling, for references."""
    starts = [0] + [i + 1 for i, byte in enumerate(source) if byte == 10]
    result = {}
    def record(location, binding):
        coordinates = [int(n) for n in re.findall(r'\d+', location)]
        if len(coordinates) != 4:
            raise ValueError('invalid parser location')
        a, b, c, d = coordinates
        bounds = starts[a] + b, starts[c] + d
        # A colon method's implicit self has no real declaration token.
        if source[bounds[0]:bounds[1]] != binding['name'].encode('utf-8'):
            return
        key = binding['name'], binding['location']
        if bounds in result and result[bounds] != key:
            raise ValueError('parser assigns two bindings to one token')
        result[bounds] = key
    def walk(node):
        if isinstance(node, list):
            for child in node:
                walk(child)
        elif isinstance(node, dict):
            if node.get('type') == 'AstExprLocal':
                record(node['location'], node['local'])
            elif node.get('type') == 'AstLocal':
                record(node['location'], node)
            else:
                for child in node.values():
                    walk(child)
    walk(root)
    return result


def validate_parser_identity(trace, source, root):
    expected = parser_occurrences(root, source)
    mapping, covered, storage = {}, set(), {}
    errors = []
    for occurrence in trace['output_map']['bindings']:
        span = occurrence['span']
        bounds = span['start']['byte_offset'], span['end']['byte_offset']
        key = expected.get(bounds)
        if key is None:
            errors.append('emitted local token is not a parser-resolved local')
            continue
        covered.add(bounds)
        bid = occurrence['binding_id']
        if key in mapping and mapping[key] != bid:
            errors.append('one parser binding maps to conflicting final IDs')
        mapping[key] = bid
        storage.setdefault(bid, set()).add(key)
    opaque = [(r['span']['start']['byte_offset'], r['span']['end']['byte_offset'])
              for r in trace['output_map']['opaque_regions']]
    missing = expected.keys() - covered
    explained = {span for span in missing if any(a <= span[0] and span[1] <= b for a, b in opaque)}
    if missing - explained and not trace['output_map']['omitted_occurrences']:
        errors.append('parser local tokens missing outside explicit opaque regions')
    return errors[:20], dict(parser_local_tokens=len(expected), mapped_local_tokens=len(covered),
                             opaque_local_tokens=len(explained), unexplained_local_tokens=len(missing - explained),
                             parser_bindings=len(set(expected.values())), mapped_parser_bindings=len(mapping),
                             storage_ids_with_multiple_parser_bindings=sum(len(keys) > 1 for keys in storage.values()))


def validate_emission_map(trace, source):
    errors = []
    def require(ok, message):
        if not ok and len(errors) < 20:
            errors.append(message)

    output = trace['output_map']
    final = {item['binding_id']: item for item in trace['final_bindings']}
    starts = [0] + [i + 1 for i, byte in enumerate(source) if byte == 10]
    require(output['schema_version'] == 1, 'unsupported output-map schema')
    require(output['omitted_occurrences'] >= 0, 'negative omitted occurrence count')
    require(sum(len(output[k]) for k in ('bindings', 'annotations', 'opaque_regions')) <= output['limits']['occurrences'],
            'output occurrence budget exceeded')

    def check_span(span):
        positions = []
        for which in ('start', 'end'):
            position = span[which]
            offset = position['byte_offset']
            require(isinstance(offset, int) and 0 <= offset <= len(source), 'output offset outside source')
            if not isinstance(offset, int) or not 0 <= offset <= len(source):
                return None
            line = bisect.bisect_right(starts, offset)
            try:
                column = len(source[starts[line - 1]:offset].decode('utf-8')) + 1
            except UnicodeDecodeError:
                require(False, 'output offset splits UTF-8 character')
                return None
            require(position['line_one_based'] == line and position['column_one_based'] == column,
                    'output line/column disagrees with byte offset')
            positions.append(offset)
        require(positions[0] < positions[1], 'empty or reversed output span')
        return tuple(positions)

    seen, ids, previous = set(), set(), -1
    for item in output['bindings']:
        bounds = check_span(item['span'])
        if bounds is None:
            continue
        start, end = bounds
        require(start >= previous, 'identifier spans unordered or overlapping')
        previous = end
        key = (item['binding_id'], start, end)
        require(key not in seen, 'duplicate identifier occurrence')
        seen.add(key)
        require(item['binding_id'] in final, 'identifier references missing final binding')
        require(item['role'] in ('read', 'assignment_target', 'declaration', 'parameter', 'iteration_binding', 'function_declaration'),
                'unknown emitted identifier role')
        if item['binding_id'] in final:
            ids.add(item['binding_id'])
            name = final[item['binding_id']]['name'] or 'UNNAMED_LOCAL'
            require(source[start:end] == name.encode('utf-8'), 'identifier bytes disagree with binding name')
            require(re.fullmatch(rb'[A-Za-z_][A-Za-z_0-9]*', source[start:end]) is not None,
                    'identifier span is not a whole identifier')
            for neighbor in (source[start-1:start] if start else b'', source[end:end+1]):
                require(not neighbor or re.fullmatch(rb'[A-Za-z_0-9]', neighbor) is None, 'identifier span clips a token')
    for item in output['annotations']:
        bounds = check_span(item['span'])
        if bounds is None:
            continue
        start, end = bounds
        require(source[start:end].startswith(b'--'), 'annotation span is not a comment')
        text = item['text'].encode('utf-8')
        require(len(text) <= output['limits']['annotation_text_bytes'], 'annotation text budget exceeded')
        payload = source[start + 3:end]
        require(source[start:start + 3] == b'-- ' and
                (payload.startswith(text) and len(payload) > len(text) if item['text_truncated'] else payload == text),
                'annotation text disagrees with source')
        require(item['classification'] == 'emitter_annotation' and item['instruction_origin'] == 'unknown',
                'annotation text promoted to instruction proof')
    for item in output['opaque_regions']:
        check_span(item['span'])
        require(item['reason'] in ('interpolated_string_argument_rendering', 'statement_display_fallback'),
                'unknown opaque-region reason')
    for field, value in {'identifier_spans': len(output['bindings']), 'annotation_spans': len(output['annotations']),
                         'opaque_output_regions': len(output['opaque_regions']),
                         'omitted_output_occurrences': output['omitted_occurrences'],
                         'bindings_without_identifier_tokens': len(final.keys() - ids)}.items():
        require(trace['summary'][field] == value, 'output map summary mismatch: ' + field)
    return errors
