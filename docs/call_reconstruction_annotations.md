# Call reconstruction events and compact annotations

Detailed binding provenance now connects reconstructed calls to their actual
output spans. A bounded producer ledger distinguishes statement de-inline,
expression de-inline, arithmetic de-inline and terminal-helper synthesis.
Each committed call creation receives an event ID, its callee binding ID at
creation and, when retained, the helper's input prototype ID. Final emission
records each surviving call occurrence and its current direct-local callee ID.

These are creation facts. A helper prototype does not identify the caller's
original instruction PCs or prove that the author used a call there. Existing
semantic, capture and close certificates remain separate and unchanged. The
ledger does not award original-source recovery credit to inferred calls.
Terminal synthesis always has a null input prototype. Comment text cannot
establish a semantic certificate.

## Output and lookup

`decompile-folder --emit-binding-provenance` writes the optional
`binding_provenance.call_reconstruction` object. Its model is
`committed-call-reconstruction-events-v1`. Event IDs are independent of stable
local IDs, so collecting diagnostics does not allocate bindings or change names.
The ledger holds only strings, integers and optional prototype IDs; it retains
no AST/local/closure owners. Call equality and debug fingerprints exclude the
event field.

Normal AST clones and the explicit deep-clone path retain the creation event.
One event can therefore have multiple emitted occurrences. Calls newly built by
another transform, including call-to-method conversions, start unattributed;
they do not inherit evidence from similar syntax. An event with no output
occurrence could have been removed, rebuilt, rendered in an opaque region or
omitted at the map budget. Absence alone does not distinguish these cases.
No event is emitted for original calls that these producers did not reconstruct.

The collector has deterministic limits of 4,096 events, 50,000 callee
registrations and 100,000 final call occurrences per artifact. Omission counts
are explicit. The occurrence budget is separate from the existing identifier
map budget. Thread-local scopes restore the previous context on nested calls,
normal completion and unwinding. Layout previews do not emit occurrences.

```powershell
luau-lifter decompile-folder input output --key 203 --emit-binding-provenance
python scripts/provenance_lookup.py --root output --script path/example.lua --byte-offset 123
```

Lookup returns matching call occurrences and their creation events alongside
identifier ancestry and annotation spans. `input_callsite` remains `unknown`.
Historical sidecars without this report remain readable and gain no inferred
classification.

## Compact display

```powershell
luau-lifter decompile-folder input output --key 203 --emit-binding-provenance --compact-annotations
```

Default source output keeps the existing full comments. Compact mode uses short
labels such as `inferred call`, `inferred arithmetic helper` and `synthesized
helper` for recognized emitter annotations. Their full original text remains
in the annotation row, with `displayed_text` describing the shortened source
payload. Other comments remain unchanged. A label is a presentation choice,
not proof of inlining in the original source.

CLI compact mode requires detailed binding provenance. The artifact API accepts
`DecompileOptions { compact_annotations: true, emit_binding_provenance: true,
..Default::default() }`; the source-only API rejects this combination because
it cannot return the metadata. Option bit 64 is included in cache identity.
The legacy single-file CLI does not expose compact mode. Opaque subrenders,
exhausted maps and annotation text beyond the 4,096-byte retention limit keep
full comments in source.

## Independent checks

The numeric validator checks budgets, unique creation-order IDs, valid retained
prototypes, synthesized-helper restrictions, exact output coordinates and
references to final bindings. It rejects invented evidence fields. The pinned
Luau parser independently confirms that every recorded occurrence is exactly
an `AstExprCall`, and that its direct local callee token agrees with the emitted
binding map. This is a location/identity check, not a proof of equivalence to
the input instructions.

`scripts/call_reconstruction_audit.py` can compare a new trace directory with
the previous release, requiring identical source bytes and every previous
sidecar field after removing only the new report. Its compact comparison
allows only the recorded comment replacements, translates every stored output
position through those replacements, checks the option-derived analysis hash
and requires all other metadata to agree. Parsed structure, binding names and
type syntax must also remain equal. Real-parser corruption controls exercise
the original emitted metadata, wrong existing callee IDs, clipped call extents,
duplicate/dangling events, duplicate occurrences, invented PCs and false budget
omissions.

`scripts/provenance_fixtures.py --compact-annotations --cache` runs normal and
compact output at one/four threads, including cold/warm artifact caches shared
between the two option modes. CI runs it on runtime and pinned public fixtures.

This completes the bounded annotation presentation and reconstructed-call
diagnostics in R6. Arbitrary value/PC provenance and a pass-complete provenance
invalidation policy remain R2 work; broader compiler recovery remains R5 work.

## Acceptance

All 180 runtime profiles and nine controls, 513 public profiles, 45 legacy
semantic configurations and 52 size gates pass. The normal runtime/public
source bytes, full dataflow results and fidelity measurements are unchanged.
All 4,629 nonempty runtime/public/private sidecars preserve every previous
field after removing only `call_reconstruction`. Private output includes
3,936 nonempty scripts and 42 skipped empty inputs. Normal and compact source
and metadata remain stable at one/four threads; runtime/public runs also
verify cold/warm artifact caches with shared cache directories across options.

The normal corpora record 855 creation events and 843 surviving call
occurrences: 747 statement, 85 expression and 23 arithmetic events. Twelve
private events have no located output occurrence and remain unclassified.
No budgets are exhausted and no event has multiple emitted occurrences in
these corpora. Native tests separately cover deep-clone repetition and actual
terminal-helper synthesis, which these corpora do not exercise. Every retained
ordinary-corpus event has a helper prototype; synthesized helpers do not gain
one from that observation.

Compact mode shortens six runtime, 36 public and 1,049 private annotations.
Every other source byte and all previous metadata facts remain unchanged after
translating positions through those comment edits. Eight real-parser controls
pass on each corpus, including the positive case. The CLI rejects compact
mode without detailed provenance. All 980 primary Rust tests plus one child
repeat and 106 Python tests pass.

Seven interleaved warm CLI rounds keep the same 3,978-file source hash for
both builds and thread counts. One-thread median is 24.448 -> 24.750 seconds
(+1.23%); 16-thread median is 1.890 -> 1.823 seconds (-3.54%). Median peak RSS
is 33,284,096 -> 33,288,192 bytes and 117,284,864 -> 117,047,296 bytes,
respectively. The 16-thread nearest-rank p95 rises from 2.176 to 2.616 seconds;
with seven samples that statistic is the maximum. These samples record cost
and variability, not a speedup or a tail-latency improvement.

The [validation inventory](roadmap_v2_acceptance/call_origin_validation.json)
records report and executable hashes, corpus comparisons, controls and the
separate uninstrumented performance measurement.
