local function fib(n)
	local a, b = 0, 1
	for _ = 1, n do
		a, b = b, a + b
	end
	return a
end
print(fib(0), fib(1), fib(10))
local function rotate(t)
	local x, y, z = t[1], t[2], t[3]
	for _ = 1, 2 do
		x, y, z = y, z, x
	end
	return x, y, z
end
print(rotate({ 1, 2, 3 }))
