-- Counted names return to the lowest free spelling of their base (`p2` ->
-- `p` once the receiver is `self`, a nested loop's `i2` -> `j`), never into a
-- name an inner function still reads from outside (`p2` stays apart from the
-- outer `p` it captures).
local Counter = {}
Counter.__index = Counter

function Counter.new(start)
	return setmetatable({ value = start }, Counter)
end

function Counter:add(amount, times)
	for _ = 1, times do
		self.value += amount
	end
	return self.value
end

local function curry(a)
	return function(b)
		return function(c)
			return a .. b .. c
		end
	end
end

local function grid(n)
	local total = 0
	for i = 1, n do
		for j = 1, n do
			for k = 1, n do
				total += i * 100 + j * 10 + k
			end
		end
	end
	return total
end

local counter = Counter.new(5)
print(counter:add(2, 3), curry("x")("y")("z"), grid(2))
