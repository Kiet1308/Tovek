-- Recursion: forward-declared mutual recursion, direct recursion, multi-value
-- tail recursion, shared-upvalue mutual recursion, tuple tailcall forwarding.
local isEven, isOdd
isEven = function(n) if n == 0 then return true end return isOdd(n - 1) end
isOdd = function(n) if n == 0 then return false end return isEven(n - 1) end
local function direct(n) if n <= 0 then return 0 end return n + direct(n - 1) end
local function fib(n) if n < 2 then return n end return fib(n - 1) + fib(n - 2) end
local function t1() return "a", "b" end
local function t2() return t1() end
print(isEven(10), isOdd(10), isEven(7), isOdd(7))
print(direct(10), fib(15))
print(t2())
local function flatten2(n)
	if n == 0 then return 0, 0 end
	local a, b = flatten2(n - 1)
	return a + 1, b + 2
end
print(flatten2(5))
local shared = 0
local ping, pong
ping = function()
	shared += 1
	if shared >= 3 then return shared end
	return pong()
end
pong = function()
	shared += 1
	return ping()
end
print(ping(), shared)