-- `a, b = b, a` on a local a closure captures, right before a generic `for`:
-- SSA folds the swap into the copies on the edge into the loop, and those
-- write and read the cell. They belong before the preparation, where the
-- bytecode swapped; kept after it they formed code between the preparation
-- and its loop that no source spells, and the function was refused. Code
-- the preparation runs (`__iter`) sees the cell as the swap left it. Found
-- by the typed fuzzer.
local function iterate(value)
	local done = false
	return function()
		if done then
			return nil
		end
		done = true
		return value
	end
end

local function swapped(x, y)
	local a, b = x, y
	local n = 0
	while n < 3 do
		n += 1
		print(x and y or a)
		a, b = b, a
		for w in iterate(a) do
			print(a, w)
		end
	end
	return function()
		return a, b
	end
end
print(swapped(1, 2)())
print(swapped(false, "y")())

local function paired(flip, list)
	local a, b = 1, if flip then 1 else -1
	local n = 0
	while n < 2 do
		n += 1
		a, b = b, a
		for _, value in ipairs(list) do
			print(value, a, b)
		end
	end
	local function get()
		return math.max(if flip then a else b, if a < 0 then 3 else 0.5)
	end
	return get()
end
print(paired(true, { 10, 20 }))
print(paired(false, {}))

local get
local observed = setmetatable({}, {
	__iter = function()
		print("iter sees", get())
		return next, {}
	end,
})
local function observe(p, q)
	local a, b = p, q
	get = function()
		return a
	end
	while a do
		print(p and q or a)
		a, b = b, a
		for _ in observed do
		end
		if not b then
			break
		end
	end
	return a, b
end
print(observe(1, false))
