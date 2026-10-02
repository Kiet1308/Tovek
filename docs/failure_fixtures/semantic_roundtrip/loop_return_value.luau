-- A value helper's `return` from inside a loop, inlined into `local x =
-- helper(...)`, stores the value and leaves through a flag and a `break`;
-- the helper's own `return` after the loop runs where the flag is still
-- set (or is the `nil` the result was declared with). Rebuilding the call
-- turns the store, the flag and the `break` back into the `return`.
local function findItem(items, name)
	for _, item in items do
		if item.Name == name then
			return item
		end
	end
	return nil
end

local function contains(list, value)
	for _, item in ipairs(list) do
		if item == value then
			return true
		end
	end
	return false
end

local function firstLong(words, size)
	for _, word in words do
		if #word >= size then
			return word
		end
	end
	print("none longer than", size)
	return "?"
end

local function equip(backpack, name)
	local tool = findItem(backpack, name)
	if tool then
		tool.Equipped = true
	end
	return tool
end

local function count(list, values)
	local hits = 0
	for _, value in values do
		if contains(list, value) then
			hits += 1
		end
	end
	return hits
end

local function label(words, size)
	local long = firstLong(words, size)
	return "[" .. long .. "]"
end

-- Keeps the last match: its store does not leave the loop.
local function last(items, name)
	local found = nil
	for _, item in items do
		if item.Name == name then
			found = item
		end
	end
	return found
end

local backpack = { { Name = "Sword" }, { Name = "Bow" }, { Name = "Sword", Extra = true } }
print(equip(backpack, "Bow").Name, backpack[2].Equipped, equip(backpack, "Axe"))
print(count({ 1, 2, 3 }, { 2, 4, 3 }), count({}, { 1 }))
print(label({ "a", "abcde" }, 4), label({ "a" }, 4))
print(last(backpack, "Sword").Extra)
