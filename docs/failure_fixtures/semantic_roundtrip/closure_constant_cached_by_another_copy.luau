-- Luau -O2 inlines `bindk` at every call below, and each copy of its
-- literal loads one constant of the enclosing function (DUPCLOSURE): the
-- argument is a local bound to a function literal, which keeps the literal
-- shareable. That constant caches: the first copy to run fills it with what
-- it captures, and a later run reuses the closure only while its own
-- captures stay rawequal to those, making a new closure otherwise. Here the
-- first loop's copy always runs first (a `for` from 1 to 2 runs a trip) and
-- fills the cache with its `h`; every other loop captures a different `h`,
-- a closure of another function, so its copy makes a new closure on every
-- trip (`false` below). The output printed one literal per copy, each with
-- a constant of its own, so every loop shared one closure (`true`). Such a
-- copy is now a new closure on every run, as the bytecode makes it: it
-- captures a private copy of its `h` (`local h2` / `h2 = h`), declared
-- right after `h` where the literal is the `until` condition, which sees
-- the loop body's locals. `make` runs twice: its first copy keeps the cache
-- it filled on the first run, the second copy stays new on every run.
-- Negative: the first loop's two copies capture the same `h`; they stay one
-- closure. Negative: `gather`'s copies all capture `shared`, declared
-- before the loops; they stay one closure across loops. Their identity is
-- not printed: compiled without inlining (the source run, and -O0/-O1),
-- `bindk`'s literal captures its parameter and is new on every call, so
-- only the comparisons that hold at every level are printed. The bytecode
-- reference harness (rtvm) checks the rest.
local function bindk(p)
	return function()
		return p
	end
end

local seen = {}
for i = 1, 2 do
	local function h()
		return 7
	end
	seen[i] = bindk(h)
	seen[i + 2] = bindk(h)
end
print(seen[2]()(), seen[3]()())

local seen2 = {}
local i = 0
repeat
	i += 1
	local function h()
		return 8
	end
	seen2[i] = bindk(h)
until i >= 2
print(seen2[1] == seen2[2], seen2[2]()())

local seen3 = {}
local j = 0
repeat
	j += 1
	local function h()
		return 9
	end
until table.insert(seen3, bindk(h)) or j >= 2
print(seen3[1] == seen3[2], seen3[2]()())

local function make(...)
	local out = {}
	for k = 1, 2 do
		local function h()
			return 1
		end
		out[k] = bindk(h)
	end
	for k = 3, 4 do
		local function h()
			return 2
		end
		out[k] = bindk(h)
	end
	return out
end
local x, y = make(), make()
print(x[3] == x[4], x[3] == y[3], x[1]()(), x[4]()())

local function gather(...)
	local function shared()
		return 3
	end
	local out = {}
	for k = 1, 2 do
		out[k] = bindk(shared)
	end
	for k = 3, 4 do
		out[k] = bindk(shared)
	end
	return out
end
local z = gather()
print(z[4]()())
