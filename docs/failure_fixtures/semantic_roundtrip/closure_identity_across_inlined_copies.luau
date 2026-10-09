-- One function literal that -O2 inlined at two call sites: both copies load
-- the same closure constant with DUPCLOSURE, which returns one cached object
-- while the captured values are equal, so `x.cb == y.cb` is true. Printed as
-- two literals, the copies recompiled to two constants and compared unequal.
-- The copies escape (stored in tables, compared, returned), so they become
-- one binding; copies only ever called keep their literals.
local function mk()
	return function() return 1 end
end
local a = mk()
local b = mk()
print(a == b, a(), b())

local function wrap(t)
	t.cb = function() return 2 end
	return t
end
local x = wrap({})
local y = wrap({})
print(x.cb == y.cb, x.cb())

-- The same literal capturing a module-level local: still one object.
local prefix = "s"
local function tag(t)
	t.get = function() return prefix end
	return t
end
local p = tag({})
local q = tag({})
print(p.get == q.get, p.get(), q.get())

-- A literal capturing a parameter is a new closure each time (Luau shares
-- only closures over top-level locals), inlined or not.
local function attach(t, v)
	t.get = function() return v end
	return t
end
local r = attach({}, "r1")
local u = attach({}, "r1")
print(r.get == u.get, r.get(), u.get())

-- A recursive local function the helper stores: every copy is the one shared
-- closure, which captures itself.
local function withStepper(t)
	local function step(k)
		if k <= 0 then return "stepped" end
		return step(k - 1)
	end
	t.step = step
	return t
end
local s1 = withStepper({})
local s2 = withStepper({})
print(s1.step == s2.step, s1.step(2))

-- A recursive local function inlined twice, only called: no identity to keep.
local function countdown(n)
	local function step(k)
		if k <= 0 then return "done" end
		return step(k - 1)
	end
	return step(n)
end
print(countdown(2), countdown(3))
