-- Deep closure/upvalue web: 3 levels of nesting, shared file-level upvalue,
-- closure-instantiated counters mutated through returned closures.
local counter = 0
local function makeCounter()
	local count = 0
	return function(step)
		count += step
		counter += 1
		return function()
			count += step
			counter += 1
			return count, counter
		end
	end
end
local c1 = makeCounter()
local bump1 = c1(2)
local bump2 = c1(5)
print(bump1(), bump1(), bump2(), bump1())
print(counter)
local function readChain()
	return function()
		return function()
			return counter
		end
	end
end
print(readChain()()())
counter = 0
print(counter, bump1())