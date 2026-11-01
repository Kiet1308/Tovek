-- Copies Luau -O2 made where an inert declaration of the caller sits
-- between a copy's argument temps and its body. `report(paths, values)`
-- inlined in the chunk evaluates its second argument, a table of
-- constants, after `find`'s written-parameter temp (`local node = root`)
-- and before its loop; `snapshot` builds its result table, a constant
-- template, after `countActive`'s `local count = 0`. Such a declaration
-- runs no code: it moves above the temps with the call, and each copy is
-- its call again (`report({...}, {...})`, `{ active = countActive(), ... }`).
local root = { Assets = { Effects = { Coin = "coin" } } }
local registry = {}
local stats = { loops = 0, cycles = 0 }

local function find(node, keys)
	for _, key in keys do
		node = node[key]
		if not node then
			return nil
		end
	end
	return node
end

local function total(values)
	local sum = 0
	for _, value in values do
		sum += value
		if sum > 100 then
			return 100
		end
	end
	return sum
end

local function report(paths, values)
	print(find(root, paths[1]), find(root, paths[2]), total(values[1]), total(values[2]))
end

local function countActive()
	local count = 0
	for _ in registry do
		count += 1
	end
	return count
end

local snapshots = 0
local function snapshot()
	snapshots += 1
	return { active = countActive(), loops = stats.loops, cycles = stats.cycles }
end

report({ { "Assets", "Effects", "Coin" }, { "Assets", "Sounds" } }, { { 1, 2, 3 }, { 60, 70 } })
local api = { snapshot = snapshot }
registry.a = true
registry.b = true
stats.loops = 2
stats.cycles = 5
local result = api.snapshot()
print(result.active, result.loops, result.cycles, snapshots)
