-- A helper with a leading statement, inlined where its value is used inside
-- another statement. The rebuilt call runs that statement where the value is
-- evaluated, so it is rebuilt only when nothing observable comes first there:
-- `record(T.read(...), clampDepth(x))` must keep the read first.
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

local store = {}
function store:set(key, value)
	print("set", key, value)
end

local out = {}
store:set("depth", clampDepth(T.read("12.5")))
out.depth = clampDepth(T.read("x"))
print(T.read("first"), clampDepth(T.read("40")))
if clampDepth(T.read("2")) > 1 then
	print("deep", out.depth)
end
