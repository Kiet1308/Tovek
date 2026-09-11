# Final identifier locations and storage ancestry

`--emit-binding-provenance` now includes `binding_provenance.output_map`.
The formatter records actual identifier token locations and final IR binding IDs
as it emits source. Parameters, local/function declarations, iteration variables,
assignment targets and expression references use the same collector. Preview
rendering never records occurrences. Ordinary source and analysis modes do not
allocate this detailed map.

Spans are half-open UTF-8 byte ranges. Line and Unicode-scalar column numbers
are one-based; a tab occupies one column in this coordinate system. The collector
keeps at most 100,000 total identifier, annotation and opaque-region occurrences
per script. Annotation text is capped at 4,096 bytes without splitting a Unicode
character. Both forms of truncation are explicit. The collector stores integer
IDs and positions, with no new local owners or identity allocation.

An exact token span links to a final binding. Its existing bounded lineage then
links to SSA definitions and their original lifted-statement PC sets. Those PC
sets describe **storage ancestry**, not the precise value producer at that use.
In the `selected` fixture, the same final binding has a definition from PC 3,
another from PC 4, and several block-parameter ancestors with no instruction
site. All remain separate in the [lookup example](roadmap_v2_acceptance/emission_example.json).
Neither coalescing nor matching names creates a source-binding or close proof.

The lookup command verifies manifest-selected metadata and source hashes before
returning the identifier, ancestors, annotation or opaque region at an offset:

```text
python scripts/provenance_lookup.py --root OUTPUT_DIRECTORY --script conditional_O2_g2.lua --byte-offset OFFSET
```

Parser declaration identity is distinct from an IR storage ID. One storage ID
can serve several lexical declarations. The independent parser audit counts
these cases; it requires all mapped occurrences of a single lexical declaration
to agree on their final ID. Swapping a reference to another same-named binding
is rejected even though a source-byte spelling check would pass.

Interpolation arguments currently render through an intermediate string. Their
temporary coordinates are not exported as final token spans: the complete
interpolation is an explicit `interpolated_string_argument_rendering` opaque
region. Display fallbacks with local references are similarly marked. Implicit
method receiver declarations have no identifier token. A missing mapping never
establishes dead code, inlining or synthesis.

Emitter comments, including de-inline/synthesis annotations, have their own
final spans and bounded text. Their classification remains `emitter_annotation`
and instruction origin remains unknown. Consumers can display this text beside
the source location; its wording alone does not establish a verified recovery
claim or an exact call-site PC.

The pinned parser independently checks every mapped token and accounts for all
unmapped local tokens:

| Dataset | Scripts with bytecode | Mapped tokens | Tokens in opaque regions | Unexplained tokens | Storage IDs with multiple lexical declarations |
|---|---:|---:|---:|---:|---:|
| Runtime fixtures | 138 | 2,271 | 0 | 0 | 0 |
| Public source matrix | 513 | 45,617 | 251 | 0 | 3 |
| Private corpus | 3,936 | 495,224 | 2,306 | 0 | 11 |

The public reuse cases are the same Fusion `Disassembly` file at O0/O1/O2,
not three independent source files. Private source has 130,447 parser bindings;
130,376 have at least one mapped token, and the remaining 71 occur entirely in
opaque regions. The private corpus also has 42 empty placeholders, outside the
metadata denominator. All 3,978 private source files, all 513 public outputs and
all 138 runtime outputs remain byte-identical to the preceding release.

All 930 primary Rust tests, one child repeat, 72 Python tests, 138 runtime
configurations with nine controls, 45 legacy semantic configurations and 52 size
gates pass. Previous non-lineage metadata is unchanged on runtime and public
matrices, and source/sidecars are deterministic at one/four threads. CI checks
both matrices with the independent parser. [Validation inventory](roadmap_v2_acceptance/emission_validation.json),
[private token audit](roadmap_v2_acceptance/emission_corpus_map.json),
[public token audit](roadmap_v2_acceptance/emission_public_map.json).

Arbitrary nested-value producer tracking, complete clone/synthesis attribution
and pass-complete provenance invalidation remain separate R2 work. This source
map provides bounded, inspectable links without promoting ancestry into proof.
