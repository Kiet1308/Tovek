# R3 inferred-role review

All examples below are inferred names. They are not claimed to be the author's original spelling. The complete rename audit keeps operators, effects, constants, binding identity, capture and type syntax unchanged.

| Context | Before | After | Known source / interpretation |
|---|---|---|---|
| A helper returns `.Width, .Height` | `local v, v2 = measures(p)` | `local width, height = measures(p)` | Result roles come from the helper's literal return fields. The fixture author's names are `leftWidth, leftHeight`; width/height is a role inference, not exact recovery. |
| Fusion ForKeys/ForPairs/ForValues passes a callback to a resolved helper | `callback` | `processor` | The helper's recorded parameter role propagates to the call argument; `processor` matches known source in these three files. |
| BufferWriter grows its backing buffer | `p: number` | `size: number` | `buffer.create` supplies size context. Source distinguishes `desiredSize` and `newSize`; decompiled storage still combines them, so `size` is a broad role. |
| BufferWriter writes string bytes | `value: string, p: number?` | `str: string, count: number?` | `str` matches source. Source uses `length`, while `count` comes from the API byte-count position. |
| Fusion helper argument for scope | `p3` | `maybeScope` | The resolved helper's role is `maybeScope`; source at this call site uses `scope`. Exact-name recovery is unchanged for this binding. |

The user reviewed the width/height, callback-to-processor and numeric-parameter-to-size examples in this session and answered “Rõ hơn” (clearer). This is a qualitative assessment of those three examples by one reader, not a population precision score. The str/count and maybeScope examples were not included in that question. Automated exact-name counts are reported separately: +9 aligned exact names on development, none lost, holdout unchanged. Human role quality must not be inferred from that count. The API/field/call rules propose local names only; dynamic fields, globals and strings are untouched.
