-- Copies Luau -O2 made of helpers returning a parameter they write
-- (`fallback = result or fallback; return fallback`): the copy's local for
-- that parameter holds the result, read after the copy (`v.Mode = mode`).
-- Each is its call again, declaring that local.
local Modes = { Fast = "fast", Slow = "slow" }

local function safeLookup(source, name, fallback)
	if not name then
		return fallback
	end
	local ok, result = pcall(function()
		return source[name]
	end)
	if ok then
		fallback = result or fallback
	end
	return fallback
end

local function getUserId(player)
	if type(player) == "table" and player.UserId then
		player = player.UserId
	end
	return player
end

local Reader = {}
function Reader.read(config)
	local out = {}
	out.Mode = safeLookup(Modes, config.Mode, "none")
	out.Speed = safeLookup(Modes, config.Speed, Modes.Slow)
	out.Owner = getUserId(config.Owner)
	return out
end

local a = Reader.read({ Mode = "Fast", Speed = "Bad", Owner = { UserId = 7 } })
local b = Reader.read({ Owner = 3 })
print(a.Mode, a.Speed, a.Owner, b.Mode, b.Speed, b.Owner)
