-- `local a, b = find(...)` takes two results; a helper returning
-- `(find(...))` gives one, so the store is no site of it.
-- a one-value helper vs a statement taking two values of the same call
local function first(s)
	local lower = string.lower(s)
	return (string.find(lower, "b"))
end

local function two(s)
	local lower = string.lower(s)
	local a, b = string.find(lower, "b")
	print(a, b)
end

print(first("aBc"))
two("xxb")
