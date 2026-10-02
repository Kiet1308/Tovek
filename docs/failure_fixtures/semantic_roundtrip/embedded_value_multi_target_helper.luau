-- A helper returning all of `find`'s results matches a store of two values;
-- one cut to one result does not.
local function locate(s)
	local lower = string.lower(s)
	return string.find(lower, "b")
end

local function firstOf(s)
	local i = locate(s)
	return i
end

function both(s)
	local a, b = locate(s)
	print(a, b)
end

print(firstOf("xxb"))
both("xxb")
