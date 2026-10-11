-- A rebuilt call changes what the function literal around it captures: it
-- drops the locals only the replaced code read and captures the helper's
-- binder. Luau shares one closure object per literal (DUPCLOSURE) when every
-- local it captures is never written and declared at the chunk's top level
-- outside loops, or bound to a literal it shares; otherwise each run makes a
-- new one. So a rebuild could flip a literal both ways (fr-deinline 1):
-- `nested`'s literal reads the written `selected` (new each call); rebuilt as
-- `read()` it would capture only `read`, a top-level helper: one object, and
-- `a == b` printed true. `outerA`'s literal captures nothing (one object);
-- rebuilt as `stepA(t)` it would capture `stepA`, which captures the
-- parameter `k`: a new object per call, and `a1 == a2` printed false. Every
-- de-inline rewrite now keeps each literal's sharing as it prints
-- (literal_sharing). The same code as an -O2 copy of the helpers is checked
-- against the bytecode (the source keeps the call there, so it shares where
-- the bytecode does not). Negative: `kept`'s literal reads the written
-- `count` beside the copy, so it stays new and `read()` is still rebuilt.
local selected = nil
local function read()
	return selected and selected.id or "missing"
end
local function nested()
	return function()
		return selected and selected.id or "missing"
	end
end
local function set(v)
	selected = v
end
local a, b = nested(), nested()
set({ id = "x" })
print(a == b, read(), a(), b())

local function outerA(k)
	local function stepA(t)
		t.x = t.x + 1
		t.y = t.x * 2
		return k
	end
	print(stepA({ x = 0 }), stepA({ x = 5 }))
	return function(t)
		t.x = t.x + 1
		t.y = t.x * 2
		return t.y
	end
end
local a1, a2 = outerA(1), outerA(2)
print("A", a1 == a2, a1({ x = 1 }), a2({ x = 2 }))

local count = 0
local function kept()
	return function()
		count += 1
		return selected and selected.id or "missing", count
	end
end
local k1, k2 = kept(), kept()
print(k1 == k2, k1(), k2())
