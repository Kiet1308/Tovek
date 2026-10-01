-- A last argument passes every value of a call unless parenthesized. The
-- parentheses go only where the callee drops the extra values anyway: a
-- local function past its last parameter, or a fixed-arity library function.
function two()
	return 16, 2
end

local function first(a)
	return a
end

local function count(...)
	return select("#", ...)
end

local function pair(a, b)
	return tostring(a) .. "/" .. tostring(b)
end

local function run()
	local v = two()
	print(first(v), count(v), pair(v))
	local w = two()
	print(type(w), math.abs(w), tonumber(w), math.max(1, w))
	local x = two()
	print(pair(1, x), string.sub("abcdef", 2, x))
	local t = {}
	local y = two()
	table.insert(t, y)
	print(#t, t[1])
end

run()
