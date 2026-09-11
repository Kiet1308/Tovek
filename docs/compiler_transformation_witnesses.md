# Pinned compiler transformation witnesses

The R5 development suite now contains all seven requested compiler families.
The fixture manifest locks source bytes, the runtime driver, expected runtime
observations and all 42 bytecode hashes before evaluating decompiler output.
It pins Luau `c2ec0d4e5ca50796ba174a7565298f59aa572268`,
`--fflags=false`, O0/O1/O2 and g1/g2. Bytecode hash checks retain register
operands, argument/result counts, capture modes and debug information.

| Family | Observed compiler transformation at O2 |
|---|---|
| Statement inlining | Two helper calls become four ordered sink calls |
| Expression inlining | Two helper calls become ordered multiply/add expressions |
| Constant argument specialization | Two constant mode arguments eliminate the helper's selection branches |
| Result alias | Identity helper call disappears; caller retains result copies |
| Early return | Two calls become branches and scalar return/assignment paths |
| Fixed-count loop | Four iterations become four ordered multiply/add steps |
| Table lowering | Mixed fields and an open result tail use NEWTABLE, stores and SETLIST |

The table case characterizes lowering at every profile; it does not claim a new
O2-only transformation. Compiler remarks independently confirm each reported
inline/unroll, and the report retains the caller prototype's full instruction
operands. The fixture source has unique lexical helper declarations. Source
and output helper call counts use those callee declaration identities rather
than regex matches in source text.

Each profile exercises 162 runtime vectors: scalar values including signed
zero, infinity and NaN, false/nil, boxed values with ordered metamethod calls,
both branch flags and errors injected at each of eight event positions. The
table case retains a nil hole and open result tail. The driver is compiled
with the subject body at the actual optimization/debug profile, so it does
not rely on the module loader's independent compiler defaults.

One deliberately incorrect source variant per family is compiled and executed
at all six profiles. The controls change an effect label, operator order,
specialization branch, return arity, nil/false result, loop order or open tail
arity. A mutant that merely fails compilation does not pass the test. All 42
compiled mutants differ from the locked observations; all 42 source/output
profiles pass. The bounded dataflow checker reports 28 `proved`, 13 `different`
and one `unknown`; successful runtime vectors do not promote the latter two.
Source and sidecars are identical at one/four decompiler threads.

At O2/g1 and O2/g2, existing passes reconstruct both statement helper calls and
both expression helper calls. The constant-specialization, identity alias and
early-return cases retain their lowered forms. This suite records that missing
coverage. It does not change matching rules, recover original call-site PCs,
establish source uniqueness or estimate precision/recall on an independent
holdout. The fixed-count loop keeps default output behavior; experimental
arithmetic loop synthesis remains opt-in.

```powershell
python scripts/compiler_witnesses.py --compiler path/luau-compile.exe --luau path/luau.exe --ast path/luau-ast.exe --lifter out/v2-call-origin-lifter.exe --keep out/compiler-witnesses --report out/compiler-witnesses.json
```

The report hashes every tool and contains original/output hashes, observations,
full dataflow results, helper counts and call-creation metadata. The retained
work directory includes disassembly, subject runners and observations. CI runs
the same manifest and controls. The [acceptance report](roadmap_v2_acceptance/compiler_witnesses.json)
records the verified development baseline; broader region normalization,
candidate search and contextual recompile validation remain separate R5 work.
