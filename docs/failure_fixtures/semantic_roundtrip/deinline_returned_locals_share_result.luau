-- Copies Luau -O2 made of a helper returning one of several locals from
-- early returns (`return cached` / `return found`): the copy evaluates each
-- of them into the result's register, so `local cached = cache[p]` is
-- `r = cache[p]` there, a `return cached` with `r` already holding it is no
-- statement at all, and the guards' rest nests (`if not r or r.Parent ~=
-- character then ... end`). Each copy is its call again.
local cache = {}
local function findHumanoid(player)
	local character = player and player.Character
	if not character then
		return
	end
	local cached = cache[player]
	if cached and cached.Parent == character then
		return cached
	end
	cache[player] = nil
	local found = character.Humanoid
	if found then
		cache[player] = found
	end
	return found
end

function torsoOf(player)
	local humanoid = findHumanoid(player)
	return humanoid and humanoid.Torso
end

function healthOf(player)
	local humanoid = findHumanoid(player)
	if humanoid then
		return humanoid.Health
	end
	return 0
end

local character = {}
character.Humanoid = { Parent = character, Torso = "torso", Health = 50 }
local player = { Character = character }
print(torsoOf(player), torsoOf(player), healthOf(player), healthOf({}), torsoOf(nil))
