-- Luau -O2 compiles a call of a local function each path of which returns
-- one value with one result even where all of a call's results would
-- spread (`isExprMultRet`). The output wrote those as `(fib(n))`; such a
-- call gives one value however it is compiled, and prints as the call it
-- is. A builtin keeps them: one replaced at run time may give more.
local function fib(n)
	local a, b = 0, 1
	for _ = 1, n do
		a, b = b, a + b
		if a > 1000000 then
			return -1
		end
	end
	local digits = tostring(a)
	if #digits > 3 then
		return tonumber(digits:sub(1, 3)) * 10 ^ (#digits - 3)
	end
	return a
end

local function grow(value)
	if value > 100 then
		return value, value * 2
	end
	return grow(value * 2)
end

print(fib(10))
print(select("#", fib(5)), fib(3))
local list = { fib(4), fib(6) }
print(#list, list[2], select("#", grow(30)), (math.max(fib(7), 3)))
