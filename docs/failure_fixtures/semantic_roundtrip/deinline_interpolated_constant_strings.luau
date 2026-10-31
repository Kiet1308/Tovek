-- Copies Luau -O2 made of `assertType(n, name, expected, value)`, whose
-- error message is an interpolated string: the constant strings passed
-- (`"spr.target"`, `"number"`) are in its text in the copy, the number
-- (`{1}`) still a hole. The pattern's text aligned with the copy's gives
-- those constants; specialized alike, each copy is `assertType(...)`.
local function assertType(argNum, fnName, expectedType, value)
	if not expectedType:find(typeof(value)) then
		error(`bad argument #{argNum} to {fnName} ({expectedType} expected, got {typeof(value)})`, 3)
	end
end

local spr = {}

function spr.target(instance, ratio, freq)
	assertType(1, "spr.target", "table", instance)
	assertType(2, "spr.target", "number", ratio)
	assertType(3, "spr.target", "number|nil", freq)
	return ratio * (freq or 1)
end

function spr.stop(instance, property)
	assertType(1, "spr.stop", "table", instance)
	assertType(2, "spr.stop", "string|nil", property)
	return property or "all"
end

-- The message without its position, which names lines of the caller.
local function attempt(...)
	local ok, message = pcall(...)
	return ok, (tostring(message):gsub("^.-:%d+: ", ""))
end

print(spr.target({}, 2, 3), attempt(spr.target, 1, 2))
print(spr.stop({}), attempt(spr.stop, {}, 5))
