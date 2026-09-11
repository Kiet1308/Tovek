# Naming candidate evidence

The legacy namer now has an optional, non-owning audit channel. `--emit-upvalue-analysis` (and the provenance mode that implies it) emits `name_inference.schema_version = 2`, including `legacy_candidates`. Ordinary decompilation keeps the existing candidate selection and source output.

Each accepted hint proposal records its stable binding ID, candidate spelling, ordinal priority, rule phase and Rust implementation location. Whole-tree usage rules identify constructor consensus, collection contents, counter/accumulator updates, boolean guards and clock deltas separately. Parameter type fallbacks are recorded even when usage evidence already wins; their existing fill-only selection policy is preserved. Conflicting Instance constructors and IsA families record invalidation events. Facts rejected inside a heuristic before it proposes a name are outside this candidate inventory.

The selected hint is the legacy base-name decision, before collision suffixing and later cleanup. It is not necessarily the final emitted spelling. The final graph still records its own before/after naming decision. A legacy row's `final_binding_present` is joined by stable identity only. An eliminated or split binding is not attached to a surviving binding by name or register resemblance.

Priorities rank deterministic rules; they are neither calibrated probabilities nor evidence of purity, totality, runtime type or source authorship. Bytecode type naming hints remain separate from recovered debug names. This work does not add source type aliases or new annotations.

The collector stores no RcLocal, Arc or function owner. This matters because unused-local detection and cleanup use strong reference counts. Raw addresses exist only as ephemeral lookup keys inside the namer and never enter metadata. Binding selection is ordered by stable ID; candidate retention is ordered by priority and a stable tie order. Limits are 50,000 bindings, 24 candidate records per binding and 256 UTF-8 bytes per candidate name. Oversized names are omitted intact. Truncation and omitted attempts are explicit, and collection limits never change source decisions.

`scripts/naming_candidates_audit.py` validates the report's identity joins, rule witnesses, winner evidence and limits. Unit tests cover losing hints, invalidation, deterministic overflow, unmapped identities, preserved fill-only type hints, unused locals and cleanup noninterference. End-to-end acceptance is recorded in `roadmap_v2_implementation.md` with report hashes.
