-- `local t = {}` takes the array part Luau predicts from the stores that
-- follow it (TableShape.cpp): a store of the next index, in source order,
-- nested blocks and closures included, or `t[i]` in a `for i = 1, k` loop
-- (k <= 16). The output printed `local v = { nil, nil, nil }` instead.
local function collect(a, b, c)
	local list = {}
	if a then
		list[1] = a
	end
	if b then
		list[2] = b
	end
	print(c)
	list[3] = c
	return list
end
local collected = collect(1, nil, 3)
print(#collected, collected[1], collected[2], collected[3])
for key, value in pairs(collect("x", "y", "z")) do
	print(key, value)
end

local function fill(n)
	local out = {}
	for i = 1, 4 do
		if i <= n then
			out[i] = i * n
		end
	end
	return out
end
local filled = fill(3)
print(#filled, filled[1], filled[3], filled[4])

local function shuffled(x)
	local t = {}
	t[2] = x
	if x then
		t[1] = x
	end
	t[3] = x
	return t
end
for key, value in pairs(shuffled(7)) do
	print(key, value)
end

local function deferred(v)
	local t = {}
	local set = function()
		t[1] = v
	end
	set()
	t[2] = v
	return t
end
local d = deferred(5)
print(#d, d[1], d[2])

local function counted(v)
	local t = {}
	if v then
		t[1] = v
		t[1] += 1
		t[2] = v
	end
	return t
end
local c = counted(4)
print(#c, c[1], c[2])
