-- `@native` sets a function prototype's LPF_NATIVE_FUNCTION flag. The flag
-- was parsed and dropped; the attribute now prints before the function's
-- `function` keyword (`@native local function`, `@native function t.f`, an
-- anonymous `@native function`), and only on the functions that carried it.
local M = {}

@native local function scale(x)
	return x * 2
end

@native function M.offset(x)
	return x + 1
end

local function plain(x)
	return x - 1
end

local lookup = setmetatable({}, {
	__index = @native function(_, key)
		return key .. "!"
	end,
})

local total = 0
for i = 1, 5 do
	total += scale(i) + M.offset(i) + plain(i)
end
print(total, lookup.abc)
