# Layered bytecode validation

The oracle first compares ordered symbolic execution trees for the supported
acyclic domain. If that model returns `unknown`, it tries a bounded transition
graph certificate, `luau-register-cfg-bisimulation-v1`. The latter accepts only
equal graphs under the stated register/prototype mapping. A graph mismatch
remains `unknown`; it is not a concrete counterexample or a claim that two
different loop implementations cannot be equivalent. A differing symbolic
tree is retained as `different`, never overridden by the fallback.

## What the graph retains

Every instruction is a node with its operation, ordered operands, exact
constants, and ordered successor edges. Branch polarity, backedges, numeric
and generic loop register groups, iterator result arity, calls, result packs,
upvalue slots, closure creation modes and CLOSE boundaries remain explicit.
The graph does not commute arithmetic, simplify expressions, remove moves,
reorder effects or assume that an API/type annotation establishes purity.

Prototype identities are assigned through a bounded queue and retained at
closure references. Distinct closure constant entries remain distinct cached
templates, even if both reference the same prototype. Sharing a template twice
is different from using two separate templates. NEWCLOSURE still creates a
fresh closure at every execution. CAPTURE words are attached to their creation
site and cannot become independent branch targets. Orphan captures and REF
captures following DUPCLOSURE refuse.

Finite register groups can be alpha-renamed consistently, with parameter slots
anchored as distinct inputs. If any reference capture occurs in the chunk, or
the entry prototype has external upvalues, the **whole chunk** instead keeps
physical register numbers and frame sizes. This avoids making an unstated
assumption about stack aliasing across nested calls. Functions with open result
packs also keep their physical layout. The existing symbolic layer continues
to cover a wider set of acyclic copy-elimination/renumbering cases.

A must-definition worklist intersects reaching definitions at joins and
converges around loops. It rejects reads without a definition on every path.
Call/iterator scratch regions are conservatively killed. Generic loop outputs
become defined on the body edge; fast-call results become defined on the
success edge. Open result state must agree at joins and be available at each
consumer. SETLIST consumes its open result state.

FASTCALL, FASTCALL1/2/2K/3 preserve the builtin ID, input operands, embedded CALL
layout/result count, success successor and complete fallback path. They are
represented as operations with effects and two outcomes, not treated as pure
substitutions for their fallback code.

Exact graph equality gives an instruction-step bisimulation: related registers,
upvalues and closure templates execute the same operations with the same
ordered inputs, effects and successors. The relation holds at joins and after
any number of loop iterations. The certificate is structural; it neither
unrolls the loop to a chosen iteration count nor attempts general equivalence
of different CFGs.

## Domain and budgets

The new graph certificate is for bytecode v9, pinned compiler/VM commit
`c2ec0d4e5ca50796ba174a7565298f59aa572268`. Newer class/feedback/native opcodes
refuse. The relevant operand and lifetime rules were checked against the
pinned [instruction definitions](https://github.com/luau-lang/luau/blob/c2ec0d4e5ca50796ba174a7565298f59aa572268/Common/include/Luau/Bytecode.h)
and [VM dispatch](https://github.com/luau-lang/luau/blob/c2ec0d4e5ca50796ba174a7565298f59aa572268/VM/src/lvmexecute.cpp).
For example, this VM uses three numeric-loop registers; the graph follows
the executable implementation rather than the stale four-register comment.

Each model receives a separate 20,000-unit budget per side. Graph input scans,
nodes, constant expansion and worklist visits consume units; string bytes also
consume units before hexadecimal expansion. Constant recursion is bounded at
32. Prototype bodies are queued instead of recursively duplicated. Exhaustion,
invalid targets, unsupported operands and unresolved packs return `unknown`.
The graph does not deserialize or execute untrusted code to test a candidate.

Debug/stack locations, instruction timing, resource exhaustion and VM garbage
collection timing/observations are outside this model. Metadata equality and
source fidelity are separate axes. A graph proof does not establish the
author's source text or validate arbitrary external API behavior.

## Oracle corrections and evidence

Integer constants now retain their signed 64-bit value rather than being
rounded to a Python float. This keeps `2^53` distinct from `2^53+1` and integer
constants distinct from floating-point constants. The acyclic checker also
retains closure constant-entry identity, uses the full comparison-register AUX
operand, and clears open result state after SETLIST.

The [implementation record](roadmap_v2_implementation.md) links the immutable
re-scoring reports and mutant tests. Re-scoring validates source/output hashes
and the pinned compiler hash, then recompiles both sides and verifies that the
original bytecode matches the saved input. It does not manufacture new runtime
observations or replace an old acceptance artifact.

```powershell
python -m unittest discover -s scripts -p 'test_*.py'
python scripts/dataflow_replay.py --fixtures-report out/baseline-runtime.json --report out/graph-runtime.json
python scripts/dataflow_replay.py --public-report out/baseline-public.json --vendor out/vendor --report out/graph-public.json
```

For the historical before/after counts, generate the input reports with the
`3a49cce` checker and then replay them with this checker. A fresh report produced
by the current `roadmap_v2.py` or `public_source_roundtrip.py` already uses both
models. Compiler options, failed/unknown cases and holdout membership stay in
the denominator.
