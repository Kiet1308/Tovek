# PR #3 re-review — residual control-flow status

This note is committed so a reviewer using only the GitHub checkout can see the
current failure state.  The input bytecode corpus is local to the development
machine; paths below are repository-relative paths from that corpus.

## Snapshot

- Repository: `Kiet1308/Tovek`
- PR: [#3](https://github.com/Kiet1308/Tovek/pull/3)
- Working branch: `fix/pet-source-like-loop-structuring`
- Reviewed code baseline: `910a2d6`; follow-up diagnostics/docs commits are on the
  same PR.
- Corpus: `D:\Medal\examplebytecode\RobloxProject`, 3,978 entries.

## Reproduced result

The current release/debug batch run is:

| result | count |
| --- | ---: |
| decompiled | 3,923 |
| empty payload skipped | 42 |
| explicit failures | 13 |

The 13 failures are fail-closed.  They do not emit a final Luau file containing
`goto`, a label, or an internal loop marker.  In analysis mode each failure has a
typed diagnostic in `.tovek-analysis/manifest.json`; the old public error text is
kept for API compatibility.

Representative command (PowerShell):

```powershell
target/release/luau-lifter.exe decompile-folder `
  D:\Medal\examplebytecode\RobloxProject `
  target\pr3-recheck `
  --key 203 --threads 8 --emit-upvalue-analysis --verbose
```

For the committed bytecode fixtures, use the command in
[`failure_fixtures/residual_control_flow/README.md`](failure_fixtures/residual_control_flow/README.md).
Those seven fixtures now decompile successfully in both default and strict
policy modes and pass the official Luau parser check.

## What is fixed in this PR

- terminal export copies are inserted before `break`/`continue`/`return`;
- mixed exit ports are rejected instead of merging incompatible environments;
- compiler generic-for break/return shapes have provenance-aware handling;
- source-like rejection reasons are typed and preserved in the batch manifest;
- legacy APIs retain their exact historical error strings;
- output is checked for residual control-flow before formatting.

## What remains

The remaining failures are not a safe reason to weaken the proof checks.  Most
`source_like_unsupported` cases contain gotos whose targets are in a sibling or
child lexical region (the lowest common ancestor is outside the current `if` or
loop).  The current label simplifier intentionally refuses those transfers.
The generic-for protocol failures additionally lack enough value/provenance data
to prove prep kind, iterator alias identity, or per-iteration captured-cell
lifetime.  A correct general fix therefore needs typed region exits, a flat
semantic CFG snapshot before AST embedding, and VM-aware generic-for protocol
proofs; routing these cases through the old matcher would be an unproven semantic
guess.

The complete path/function matrix and concrete failure examples are in
[`Tovek_PR3_Residual_Control_ReReview_718d17f.md`](Tovek_PR3_Residual_Control_ReReview_718d17f.md).

