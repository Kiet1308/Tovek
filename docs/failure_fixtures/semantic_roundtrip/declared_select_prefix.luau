-- A helper with a leading local whose returns fuse into `not n and 4 or
-- (math.clamp(...))` at its inlined sites; the rebuilt calls must keep each
-- site's argument, including one that runs code.
local T = {}
function T.read(value)
	print("read", value)
	return value
end

local function clampDepth(value)
	local n = tonumber(value)
	if not n then
		return 4
	end
	return math.clamp(math.floor(n), 1, 32)
end

local a = clampDepth(T.read("7.9"))
local b = clampDepth(T.read("x"))
local c = clampDepth(a * 10)
if a ~= c then
	print("changed", c)
end
print(a, b, c)
