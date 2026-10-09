-- A rebuilt call whose result one statement reads, first of all it
-- evaluates after nothing but reads the call cannot change, moves into that
-- read, as the source called it: an argument (`assert(ready(x), msg)`), a
-- chain of them (`mix(scale(a), scale(b))`, folded from the last), the last
-- argument of a format.
-- Negative: `report` prints `counter` before the result, and the helper
-- changes `counter`: moved in, the call would run after that read, so the
-- local stays.
local counter = 0
local function ready(x)
	return type(x) == "table" and x.ready == true
end
local function scale(v)
	return math.floor(v * 2) + math.max(v, 1)
end
local function mix(a, b)
	return a .. "+" .. b .. "="
end
local function label(n)
	counter += 1
	return "#" .. tostring(n) .. ":" .. counter
end

local M = {}
function M.check(x)
	assert(ready(x), "not ready")
	return "ok"
end
function M.mix(a, b)
	local x = scale(a)
	local y = scale(b)
	return mix(x, y)
end
function M.text(n)
	local shown = n * 2
	return ("value %* of %*"):format(shown, label(n))
end
function M.report(n)
	local r = label(n)
	print(counter, r)
	return r
end

print(M.check({ ready = true }), pcall(M.check, {}) == false)
print(M.mix(1, 20))
print(M.text(5), M.report(7), counter)
