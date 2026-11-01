-- A helper returning its own local after a loop it returns from
-- (`local node = root; for ... do node = node[name]; if not node then
-- return nil end end; return node`): Luau keeps the local in the result's
-- register, so a copy stores each loop return into it and breaks (`node =
-- nil; break`, under a flag where code follows) and its last `return` has
-- nothing to store. Unified with the exits given back as returns, each
-- copy is `local node = find(...)` again.
local tree = { Assets = { Effects = { Coin = "coin" } } }

local function find(root, path)
	local node = root
	for _, name in path do
		node = node[name]
		if not node then
			return nil
		end
	end
	return node
end

local function total(list)
	local sum = 0
	for _, value in list do
		sum += value
		if sum > 100 then
			return 100
		end
	end
	return sum
end

local function report(paths, lists)
	local coin = find(tree, paths[1])
	local missing = find(tree, paths[2])
	local first = total(lists[1])
	local second = total(lists[2])
	print(coin, missing, first, second)
end

report({ { "Assets", "Effects", "Coin" }, { "Assets", "Sounds" } }, { { 1, 2, 3 }, { 60, 70 } })
