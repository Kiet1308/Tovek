"""Validate explicit emitter introductions without inventing input provenance."""
import re

MODEL = 'committed-emitter-local-introductions-v1'
RECORD_LIMIT = 4096
PASSES = {
    'branch_constructors': ('luau-v9-private-property-diamond-v2',
                            {'constructor_property_value', 'constructor_initializer_snapshot'}),
    'conditional_lowering': ('luau-v9-scalar-select-statements-v1',
                             {'scalar_select_result', 'short_circuit_result', 'evaluation_snapshot'}),
}


def validate_local_producers(trace, metadata=None):
    errors = []
    def require(test, message):
        if not test and len(errors) < 20:
            errors.append('local producer: ' + message)
    def count(value):
        return type(value) is int and value >= 0
    final = {row['binding_id']: row for row in trace['final_bindings']}
    ledger = trace.get('local_producers')
    if ledger is None:
        require(not any('emitter_introduction' in row for row in final.values()), 'pointer without ledger')
        return errors  # Historical sidecars remain readable, with no synthesis claim.
    try:
        require(ledger['schema_version'] == 1 and ledger['model'] == MODEL, 'unsupported ledger model')
        require(ledger['records_per_pass'] == RECORD_LIMIT, 'changed record budget')
        passes = ledger['passes']
        require(type(passes) is list and len(passes) <= len(PASSES), 'invalid pass inventory')
        if errors:
            return errors
        recorded = {r['binding_id']: r for r in (metadata or {}).get('source_recovery', {}).get('bindings', [])}
        pass_names, introduced = set(), set()
        omitted = 0
        for group in passes:
            name = group['pass']
            require(name in PASSES and name not in pass_names, 'unknown or repeated pass')
            if name not in PASSES:
                continue
            pass_names.add(name)
            model, roles = PASSES[name]
            require(group['rewrite_model'] == model, 'unknown rewrite model')
            records = group['records']
            require(type(records) is list and len(records) <= RECORD_LIMIT, 'record budget exceeded')
            require(count(group['introduced_locals']) and count(group['omitted_records']), 'invalid introduction counts')
            if errors:
                return errors
            require(group['introduced_locals'] == len(records) + group['omitted_records'], 'unaccounted introductions')
            require(len(records) == min(RECORD_LIMIT, group['introduced_locals']), 'premature record omission')
            omitted += group['omitted_records']
            previous = -1
            for index, record in enumerate(records):
                bid = record['binding_id']
                valid_id = type(bid) is str and re.fullmatch(r'b(?:0|[1-9]\d{0,19})', bid) is not None
                require(valid_id, 'invalid binding ID')
                if not valid_id:
                    continue
                numeric = int(bid[1:])
                require(previous < numeric < 2 ** 64, 'unordered or out-of-range binding ID')
                previous = numeric
                require(record['role'] in roles, 'role not supported by producer pass')
                require(bid not in introduced and bid in final, 'duplicate or missing final binding')
                if bid not in final:
                    continue
                require(not final[bid]['lineage'], 'input ancestry copied to introduced local')
                require(not final[bid].get('recorded_source_origins') and not recorded.get(bid, {}).get('origins'),
                        'recorded source identity copied to introduced local')
                require(final[bid]['incomplete'], 'missing input ancestry claimed complete')
                introduced.add(bid)
                pointer = {'pass': name, 'record': index}
                require(final[bid].get('emitter_introduction') == pointer, 'forward introduction link changed')
            if metadata is not None:
                report = metadata.get(name)
                require(type(report) is dict, 'missing pass report')
                if type(report) is dict:
                    require(report.get('model') == model and report.get('introduced_locals') == group['introduced_locals']
                            and report.get('introduced_bindings') == dict(records=records, omitted_records=group['omitted_records']),
                            'ledger differs from committed pass report')
        for bid, row in final.items():
            if 'emitter_introduction' in row:
                require(bid in introduced, 'reverse introduction link has no record')
        require(count(ledger['recorded_introductions']) and ledger['recorded_introductions'] == len(introduced),
                'recorded count differs')
        require(count(ledger['omitted_records']) and ledger['omitted_records'] == omitted, 'omission count differs')
        if metadata is not None:
            require(pass_names == {name for name in PASSES if metadata.get(name) is not None}, 'pass coverage differs')
    except (KeyError, TypeError, ValueError, AttributeError):
        errors.append('local producer: malformed ledger')
    return errors


def introductions(trace):
    """Caller must validate first. Absence from this map conveys no origin fact."""
    return {record['binding_id']: {'pass': group['pass'], 'rewrite_model': group['rewrite_model'], 'role': record['role']}
            for group in trace.get('local_producers', {}).get('passes', []) for record in group['records']}
