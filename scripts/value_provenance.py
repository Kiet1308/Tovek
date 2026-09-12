"""Validate nested input origins and bounded emitted dependency regions.

This checks the stated ancestry contract, never semantic equivalence or an
original source-variable claim. Missing exact producers must stay unknown.
"""

import collections


def summarize(trace):
    """Coverage only; percentages must retain unknown/omitted denominators."""
    report = trace.get('value_provenance')
    if report is None: return {}
    counts = collections.Counter(
        input_values=sum(len(f.get('value_origins', [])) for f in trace['functions']),
        output_regions=len(report['output_regions']),
        omitted_output_regions=report['omitted_output_regions'],
        compiler_loop_control_definitions=sum(d['kind'] == 'compiler_loop_control_definition'
            for f in trace['functions'] for d in f['definitions']),
        phi_transport_maps=sum(m['phase'] in ('phi_parameter_transport', 'phi_edge_transport')
            for f in trace['functions'] for m in f['local_maps']))
    for region in report['output_regions']:
        counts['storage_' + region['relation']] += 1
        node = region.get('node_ancestry', {})
        counts['node_' + node.get('relation', 'unrecorded')] += 1
        counts['node_inlined'] += bool(node.get('inlined'))
        counts['node_cloned'] += bool(node.get('cloned'))
        counts['node_multi_origin'] += len(node.get('inputs', [])) > 1
        counts['node_incomplete'] += bool(node.get('incomplete', True))
        counts['node_kind_' + region['kind']] += 1
    for binding in report['bindings']:
        for role in binding['classifications']: counts['binding_' + role] += 1
    return dict(sorted(counts.items()))


def validate(trace, source=None):
    report = trace.get('value_provenance')
    if report is None:  # Historical schema-1 artifacts remain readable.
        return []
    errors = []
    boundaries = None
    if source is not None:
        boundaries = {0}
        offset = 0
        for character in source.decode('utf-8'):
            offset += len(character.encode('utf-8'))
            boundaries.add(offset)

    def require(ok, reason):
        if not ok and len(errors) < 20:
            errors.append(reason)

    require(report['schema_version'] == 1, 'unsupported value provenance')
    final = {row['binding_id'] for row in trace['final_bindings']}
    all_sites = {}
    functions_by_id = {}
    for index, function in enumerate(trace['functions']):
        if 'function_id' in function:
            key = function['function_id']
            require(key not in functions_by_id, 'duplicate input function identity')
            functions_by_id[key] = function
        sites = {(s['block'], s['statement_index']): s for s in function['lifted_statements']}
        for (block, statement), site in sites.items():
            all_sites[f'f{index}:b{block}:s{statement}'] = site
        nodes = function.get('value_origins', [])
        require([n['id'] for n in nodes] == list(range(len(nodes))), 'value IDs not contiguous')
        paths = set()
        known = {row['id'] for row in function['registers'] + function['definitions']}
        for node in nodes:
            location = (node['block'], node['statement_index'])
            key = (*location, tuple(node['path']))
            require(key not in paths, 'duplicate value path')
            paths.add(key)
            require(len(node['path']) <= 259 and node['path'][0] in (0, 1), 'invalid value path')
            if not function['dropped_records']:
                require(location in sites, 'nested value has no instruction cluster')
                require(node['binding_id'] is None or node['binding_id'] in known, 'unknown nested SSA binding')
            for child in node['children']:
                valid = node['id'] < child < len(nodes)
                require(valid, 'cyclic or missing value child')
                if valid:
                    nested = nodes[child]
                    require(nested['path'][:-1] == node['path'] and
                            (nested['block'], nested['statement_index']) == location,
                            'child escaped input value occurrence')
        for event in function.get('inline_events', []):
            require(event['exact_final_value_mapping'] is False, 'inline promoted to exact final producer')
    require(set(report['source_sites']) == set(all_sites), 'source site inventory differs')
    for key, site in report['source_sites'].items():
        expected = all_sites.get(key)
        if expected:
            require(site['instruction_pcs'] == expected['instruction_pcs'] and
                    site['source_lines'] == expected['source_lines'], 'invented input PC or line')
    require(len(report['output_regions']) <= report['limits']['output_regions'], 'region budget exceeded')
    require(report['visited_dependencies'] <= report['limits']['work'], 'dependency work budget exceeded')
    rows = {row['binding_id']: row for row in report['bindings']}
    require(set(rows) == final, 'binding projection inventory differs')
    for row in [*rows.values(), *report['output_regions']]:
        require(len(row['source_sites']) <= report['limits']['sites_per_region'], 'site budget exceeded')
        require(set(row['source_sites']) <= all_sites.keys(), 'unknown source site')
    for region in report['output_regions']:
        require(region['exact_value_producer'] is False, 'dependency promoted to exact producer')
        require(region['start_byte'] <= region['end_byte'], 'reversed output region')
        require(set(region['bindings']) <= final, 'region references missing binding')
        expected = sorted({site for binding in region['bindings'] if binding in rows
                           for site in rows[binding]['source_sites']})[:report['limits']['sites_per_region']]
        require(region['source_sites'] == expected, 'region dependency union differs')
        require(region['relation'] == ('storage_dependency_ancestry' if expected else 'unknown'),
                'unknown region promoted to input origin')
        require(bool(expected) or region['incomplete'], 'unattributed region claimed complete')
        node = region.get('node_ancestry')
        if node is not None:
            require(node['exact_value_producer'] is False, 'node ancestry promoted to exact value identity')
            require(len(node['inputs']) <= report['node_input_limit'], 'node origin budget exceeded')
            keys = []
            for origin in node['inputs']:
                site = report['source_sites'].get(origin['source_site'])
                require(site is not None, 'node references unknown input site')
                if site is None: continue
                require(site['function_id'] == origin['function_id'], 'node escaped its input function')
                key = (origin['function_id'], site['block'], site['statement_index'], origin['value_origin'])
                require(key not in keys, 'duplicate node input origin')
                keys.append(key)
                if origin['value_origin'] is not None:
                    values = functions_by_id.get(origin['function_id'], {}).get('value_origins', [])
                    index = origin['value_origin']
                    valid = type(index) is int and 0 <= index < len(values)
                    require(valid, 'unknown retained value occurrence')
                    if valid:
                        require((values[index]['block'], values[index]['statement_index']) ==
                                (site['block'], site['statement_index']), 'retained value escaped input statement')
            expected_relation = ('retained_node_ancestry' if node['inputs'] else
                                 'synthesized_node' if node['synthesized_by'] else 'unknown')
            require(node['relation'] == expected_relation, 'unsupported node origin relation')
            require(node['synthesized_by'] in (None, 'statement_deinline', 'expression_deinline',
                                             'arithmetic_deinline', 'terminal_synthesis'), 'unknown node synthesis producer')
            require(expected_relation != 'unknown' or node['incomplete'], 'missing node origin claimed complete')
            require(type(node['cloned']) is bool and type(node['inlined']) is bool, 'invalid node history flags')
            require(not (node['cloned'] or node['inlined']) or expected_relation != 'unknown' or node['incomplete'],
                    'node history has no evidence')
        if source is not None:
            start, end = region['start_byte'], region['end_byte']
            require(0 <= start <= end <= len(source), 'output region outside source')
            require(start in boundaries and end in boundaries, 'output region splits UTF-8')
    return errors
