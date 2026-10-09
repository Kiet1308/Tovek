-- A value helper whose last `return false` closes two paths (`isCallable`):
-- canon places a copy of that `return` at each path falling off the `if`
-- before it (return distribution), the shape Luau's -O2 copy stores the
-- result in. Its copies rebuild where the value lands: an argument
-- (`assert(isCallable(f), msg)`), a condition, the value of `P or
-- isCallable(f)` with `P` a comparison (Luau jumps from `P` to the copy's
-- store of `true`; `P` boolean, the split is exact), and the caller's own
-- `return` (a helper returning exactly one value on every path; the copy's
-- `return true` / `return false` cloned into the caller).
-- Negatives: `loose` tests a field, not a comparison, before the copy it
-- wrote by hand: `t.flag or isCallable(x)` would give the field's value,
-- not `true`, so it stays as written. `maybe` may return nothing: its
-- result's local stays where all of a value's results are taken, as
-- `select("#", maybe(x))` could count no value at all.
local function isCallable(value)
	if type(value) == "function" then
		return true
	end
	if type(value) == "table" then
		local metatable = getmetatable(value)
		if metatable and type(rawget(metatable, "__call")) == "function" then
			return true
		end
	end
	return false
end

local function maybe(x)
	if type(x) == "number" then
		return x * 2
	end
end

local M = {}

function M.check(f, msg)
	assert(isCallable(f), msg)
	return "checked"
end

function M.optional(f)
	return f == nil or isCallable(f), "optional"
end

function M.andThen(x)
	return isCallable(x.andThen)
end

function M.run(x)
	if isCallable(x) then
		return x(2)
	end
	return "plain"
end

function M.loose(t, x)
	local v
	if t.flag or type(x) == "function" then
		v = true
	elseif type(x) == "table" then
		local metatable = getmetatable(x)
		v = metatable and type(rawget(metatable, "__call")) == "function" and true or false
	else
		v = false
	end
	return tostring(v)
end

function M.count(x)
	local r = maybe(x)
	return select("#", r)
end

local callable = setmetatable({}, { __call = function(_, n) return n + 1 end })
print(M.check(print, "m"), (pcall(M.check, 5, "not callable")))
print(M.optional(nil), M.optional(callable), M.optional(4))
print(M.andThen({ andThen = callable }), M.andThen({ andThen = 1 }), M.andThen({}))
print(M.run(function(n) return n * 10 end), M.run(callable), M.run({}))
print(M.loose({ flag = 7 }, 1), M.loose({}, print), M.loose({}, 3))
print(M.count(4), M.count("x"))
