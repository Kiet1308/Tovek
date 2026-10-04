-- `debug.info(1, "f")` answers with the function running it. Code inlined
-- from `frameIs` must not become a call to it again: the caller's copy reads
-- the caller's frame.
local function frameIs(expected)
	return debug.info(1, "f") == expected
end

local current = debug.info(1, "f")
local result = debug.info(1, "f") == current

print(result)
return frameIs, current, result
