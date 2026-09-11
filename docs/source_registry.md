# Verified upstream source lookup

The optional source registry is separate from the decompiler. It builds from
the pinned, license-bearing [public source manifest](source_corpus_v2.json) and
[compiler profiles](source_registry_v2.json). Ordinary decompilation and its
fallback source are unchanged. A source lookup is labelled **matched upstream
source**; comments, types and author names in it belong to that upstream source
and are not claimed recovered from input bytecode.

Each entry records source SHA-256, repository commit and URL, license identity,
license bytes/hash, compiler binary hash, expected compiler commit, complete
profile and compiled bytecode hash. The registry copies source, licenses and
bytecode into content-addressed artifacts. Lookup validates their hashes and
compares complete canonical execution images after a hash narrows candidates.
An accepted candidate is freshly recompiled from its stored source. A forged
source-to-bytecode relation fails this check even if the registry's own hashes
have been recomputed.

The exact image model supports bytecode v9/type-info v3 only. It retains every
decoded opcode, register, operand and AUX bit, constant bytes including float
NaN payloads and negative zero, the entire ordered string pool, prototype
topology, function name, native flags and type payloads. It normalizes opcode
encoding and excludes debug line/local tables. Those metadata differences are
reported; debug line/stack reflection is outside the compatibility claim.
Other versions and unknown constant tags refuse lookup. There is no fuzzy text,
opcode multiset or register-erasing fallback.

Five profiles cover O0/O1/O2 with g1, plus O2 native compilation with and without
the declared Vector3 compiler configuration. All disable feature flags with
`--fflags=false`. A native profile prepends `--!native` and retains the resulting
native/type metadata exactly. The preamble is a declared compatibility-profile
addition, not a recovered author directive. For example, the private
`springCoefficients` input matches its native profile; dropping native flags or
type payloads would hide a real input difference.

Inputs normally require a bare serialized chunk. `--trailer-bytes 24` explicitly
allows either no trailer or exactly 24 opaque trailing bytes. The corpus has
417 bare chunks and 3,519 with this trailer, plus 42 empty placeholders. Each
trailer length/hash is reported; its container meaning or authentication is
not established. The pinned Luau loader consumes the serialized prefix through
the main-prototype ID. This option does not generalize to unknown extensions.

Admission requires at least eight instructions and four substantive operations;
NOP, PREPVARARGS, RETURN, LOADNIL and COVERAGE do not count as substantive.
This is a conservative low-information filter, not statistical entropy. A full
image matching multiple distinct source hashes always reports ambiguity.
Several compiler profiles of the same source text can support one match.

```text
python scripts/source_registry.py build --vendor VENDOR --compiler COMPILER --registry REGISTRY
python scripts/source_registry.py query --registry REGISTRY --compiler COMPILER --corpus DUMP_FOLDER --key 203 --trailer-bytes 24 --report REPORT.json
```

Add `--materialize OUTPUT_FOLDER` to the query to write verified matches into
separate `.matched.luau` files. Each has a visible registry label and a sibling
JSON file containing the upstream commit, profile, source hash and known
differences. License artifacts accompany the output. Materialization recompiles
the labelled source and requires the same execution image before writing.
Existing different files are never overwritten. Source availability in the
target Roblox module hierarchy is not inferred from an image match.

There are limits of 2,000 source files, eight profiles, 16 MiB per input/artifact,
64 MiB of distinct registry artifacts, 128 MiB of cached canonical images,
50,000 prototypes and one million instructions per image. Lookup allows at
most 64 candidate entries for selection; compilation has a 30-second timeout.
Canonical images are shared across duplicate artifacts. Concurrent artifact
writes and their path-containment checks are serialized.

The registry contains **171 sources and 855 configurations**. An independent
recompile/lookup audit passes all 855, including expected refusals:

| Family | Sources | Accepted configurations | Low information | Ambiguous source text |
|---|---:|---:|---:|---:|
| Fusion | 65 | 307 | 18 | 0 |
| RbxUtil | 53 | 255 | 10 | 0 |
| Rodux | 15 | 56 | 17 | 2 |
| Promise | 1 | 5 | 0 | 0 |
| Roact | 37 | 181 | 4 | 0 |

The two real ambiguous entries are Rodux `types/reducers.lua` and
`types/store.lua` at O0. This matrix checks the registry against its catalog;
it is not a holdout generalization or Roblox runtime score, even for the
repository otherwise held out from decompiler heuristic development.

Private-corpus lookup verifies **7/3,936 nonempty files (0.18%)**, all Fusion:
`getTweenDuration`, `getTweenRatio`, `springCoefficients`, `messages`,
`needsDestruction`, `isSimilar` and `xtypeof`. They are seven distinct source
texts. There are 3,633 unmatched and 296 low-information refusals; 42 empty
placeholders are counted separately. The remaining 3,929 nonempty files may
contain libraries or custom code and are not classified merely by a failed
lookup. All six historical return-nil collisions refuse selection. Of eleven
older nontrivial candidates, seven verify, two sRGB files do not match the
stricter image, and two tiny utilities fail the admission threshold.

Seven matched private outputs were materialized separately, rechecked against
current input hashes and recompiled with their recorded profiles. The default
decompiler source is untouched. Two fresh registry builds produce identical
index bytes. All 80 Python tests and 28 real-compiler controls pass, covering
operand/result/store changes, native/type preservation, source forks, distinct
comment variants with equal images, trivial nil, corrupt source/license blobs,
forged source-image relations and labelled materialization. CI runs the controls
and the full public registry matrix. [Validation inventory](roadmap_v2_acceptance/registry_validation.json),
[corpus results](roadmap_v2_acceptance/registry_corpus.json),
[public audit](roadmap_v2_acceptance/registry_audit.json).

Coverage supports keeping this as an optional tool. It does not replace binding,
effect or compiler-transform recovery, and does not justify automatically
expanding the catalog or substituting unverified library versions.
