-- A helper returning a value from inside a loop, inlined at -O2, leaves the
-- loop through a jump past the rest of the helper: the result register is
-- written and, for a numeric `for`, coalesced with the counter that the
-- post-loop code then reads, while the exhaustion path stores the helper's
-- tail value. Every shape below once failed to decompile ("residual goto").
local function indexOf(list, value)
	for i = 1, #list do
		if list[i] == value then
			return i
		end
	end
	return nil
end

local function lastIndexOf(list, value)
	for i = #list, 1, -1 do
		if list[i] == value then
			return i
		end
	end
	return 0
end

local function firstOver(list, limit)
	for i = 1, #list do
		if list[i] > limit then
			return i
		end
	end
end

local function indexOrCount(list, value)
	for i = 1, #list do
		if list[i] == value then
			return i
		end
	end
	print("missing", value)
	return -#list
end

local function contains(list, value)
	for i = 1, #list do
		if list[i] == value then
			return true
		end
	end
	return false
end

local function classify(list, value)
	for i = 1, #list do
		if list[i] == value then
			return "equal"
		elseif list[i] > value then
			return "greater"
		end
	end
	return "none"
end

local function findPair(list, value)
	for i = 1, #list do
		if list[i] == value then
			return i, list[i] * 2
		end
	end
	return nil, 0
end

-- The helper's own `break` reaches its tail, a `return` skips it.
local function scanUntilZero(list, value)
	for i = 1, #list do
		if list[i] == 0 then
			break
		end
		if list[i] == value then
			return i
		end
	end
	return -1
end

local function scanPairs(list, value)
	for i, item in list do
		if item == 0 then
			break
		end
		if item == value then
			return i
		end
	end
	return -1
end

local function one(list, value)
	local at = indexOf(list, value)
	local last = lastIndexOf(list, value)
	local over = firstOver(list, value)
	print(at, last, over)
end

local function two(list, value, other)
	local a = indexOrCount(list, value)
	local b = indexOrCount(list, other)
	if contains(list, value) then
		print("has", value)
	end
	print(a, b, classify(list, value), classify(list, other))
end

local function three(lists, value)
	for k, list in lists do
		local at = indexOf(list, value)
		if at then
			print("found in", k, at)
			break
		end
	end
	for k = 1, #lists do
		local i, doubled = findPair(lists[k], value)
		print(k, i, doubled)
	end
	local n = 0
	while n < 2 do
		n += 1
		print(n, scanUntilZero(lists[n], value), scanPairs(lists[n], value))
	end
end

one({ 5, 3, 8, 3 }, 3)
one({}, 1)
two({ 1, 4, 9 }, 4, 7)
two({ 2 }, 2, 1)
three({ { 1, 2 }, { 7, 0, 7 }, { 7 } }, 7)
three({ { 0, 7 }, { 3, 5 } }, 5)
