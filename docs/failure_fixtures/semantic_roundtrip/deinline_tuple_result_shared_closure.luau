-- Regression fixture (m3-integrate adversarial check, 2026-10-11): helpers returning several
-- values, one of them a function literal Luau shares (DUPCLOSURE: it captures nothing).
-- -O2 inlines `mk` into `run` and `run` into the chunk twice; every copy of the literal there
-- loads the chunk's one closure constant, so a1, b1, a2 and b2 are one object. The straight-tuple
-- pattern of `run` declares its results as fresh locals and drops the `return`, so they looked like
-- private local functions (no read but calls, here none) and one copy of `run` was rebuilt as a
-- call: its results became `run`'s own constant while the copy left inline kept the chunk's, and
-- `a1 == a2` printed false. A result reaches the caller and is never private: such copies stay
-- inline (`hand` beside them too). `pair` returns its own local function the same way. `split`,
-- returning no closure, is still rebuilt.
local function mk()
	return function() return 1 end
end
local function run()
	local hand = function() return 1 end
	local a = mk()
	return a, mk(), hand
end
local function pair()
	local function g() return 2 end
	return g, 3
end
local function split(t, k)
	local head = t.items[k]
	t.count = t.count + 1
	return head, t.items[k + 1]
end
local a1, b1, h1 = run()
local a2, b2, h2 = run()
print(a1 == b1, a1 == a2, b1 == b2, h1 == h2, h1 == a1, a1() + b2() + h2())
local g1, n1 = pair()
local g2, n2 = pair()
print(g1 == g2, g1() + n1 + n2)
local t = { items = { "a", "b", "c" }, count = 0 }
local x, y = split(t, 1)
local z, w = split(t, 2)
print(x, y, z, w, t.count)
