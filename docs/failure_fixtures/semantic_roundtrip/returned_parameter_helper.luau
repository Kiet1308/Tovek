-- A helper returning its own local and then its parameter: the second
-- result is the caller's argument, so a rebuilt call declares only the
-- first (`local names = toNames(children)`), and the caller keeps reading
-- its argument. A written parameter is not the argument any more.
local function toNames(list)
	local names = {}
	for i = 1, #list do
		names[i] = list[i].Name .. "!"
	end
	return names, list
end

local function bump(list, extra)
	local count = #list
	extra = extra + count
	return count, extra
end

local function children(n)
	local out = {}
	for i = 1, n do
		out[i] = { Name = "c" .. i }
	end
	return out
end

local function run(n)
	local names, items = toNames(children(n))
	print(#names, names[1], #items, items[1].Name)
	local count, total = bump(items, 10)
	print(count, total)
end

run(3)
