-- Value helpers whose copy starts with statements of the helper before the
-- value, rebuilt now that such a prefix may be any length and hold
-- branches (the value-site locator had a limit of four plain statements):
-- `makeNode`, six statements then `return node`, and `getParent`, whose
-- prefix holds an `if`, both returning their own local. `resolve` returns
-- its own local on one path and other values on the rest: Luau computes
-- that local in the result's register, so its copy has nothing to store
-- there (`local v = E; if not v then ... v = x end`), rebuilt as
-- `local v = resolve(clip, path)`. `first` returns `nil` on one path.
-- Negative: `track` returns either of two of its own locals, both in the
-- result's register: no one helper local stands for it, the copy stays.
local cache = { b = "cached" }
local log = {}
local function lookup(path)
	return "L:" .. tostring(path)
end
local function makeNode(parent, name)
	local node = {}
	node.Name = name
	node.Anchored = true
	node.CanCollide = false
	node.Transparency = 1
	node.Parent = parent
	return node
end
local function getParent(store)
	local display = store.display
	if not display then
		display = { Name = "Display" }
		store.display = display
	end
	return display
end
local function resolve(clip, path)
	local v = type(clip.overrides) == "table" and clip.overrides[path] and lookup(clip.overrides[path])
	if v then
		return v
	end
	if cache and cache[path] then
		return cache[path]
	end
	return lookup(path)
end
local function first(t)
	if not t then
		return nil
	end
	return string.upper(t.name) .. "!"
end
local function track(player)
	local character = player and player.character
	if not character then
		return nil
	end
	local cached = cache[player]
	if cached and cached.parent == character then
		return cached
	end
	local found = character.humanoid
	if found then
		cache[player] = found
	end
	return found
end
local M = {}
function M.build(root)
	local a = makeNode(root, "A")
	table.insert(log, a.Name)
	local store = {}
	local part = makeNode(store, "B")
	part.Parent = getParent(store)
	return a.Parent == root, part.Parent.Name, store.display == part.Parent
end
function M.paths(clip)
	local out = {}
	for _, path in { "a", "b", "c" } do
		local inst = resolve(clip, path)
		table.insert(out, inst)
	end
	return table.concat(out, ",")
end
function M.firsts(t)
	local r = first(t)
	table.insert(log, tostring(r))
	return r
end
function M.health(player)
	local h = track(player)
	return h ~= nil and h.hp > 0
end
print(M.build({}))
print(M.paths({ overrides = { a = "x" } }), M.paths({}))
print(M.firsts(nil), M.firsts({ name = "q" }), table.concat(log, ";"))
local hum = { hp = 5 }
local char = { humanoid = hum }
hum.parent = char
print(M.health({ character = char }), M.health({ character = char }), M.health(nil))
