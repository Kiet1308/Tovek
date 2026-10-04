-- `observe` reads its caller's frame. Code calling it, moved into a helper,
-- would hand it the helper's frame instead. (`...` keeps the compiler from
-- inlining `observe`.)
local function observe(...)
	return debug.info(2, "f")
end

local function frameIs(expected)
	return tostring(observe() == expected) .. " frame"
end

local current = debug.info(1, "f")
local result = tostring(observe() == current) .. " frame"

print(result)
return observe, frameIs, current, result
