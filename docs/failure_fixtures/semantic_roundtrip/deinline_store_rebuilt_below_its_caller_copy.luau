-- A value helper whose copy holds the copy of another: Luau's -O2 inlines
-- `getTargets` into `Circle` and `Full`, and `getEnemies` into each copy of
-- it (`getUnits` stays a call), so the site reads `local v; if ... then
-- v = getUnits(data) else v = <getEnemies copy> end`. Nothing matches in
-- the de-inliner's first scan. The assignment phase that follows rebuilds
-- `v = getEnemies(data)` inside the arm first; only then does the enclosing
-- copy unify with `getTargets`, in the same scan, rebuilt as `local targets
-- = getTargets(data)`. A shortcut that skips the plain matchers wherever the
-- previous scan found nothing must therefore stop as soon as anything below
-- or inside the block was rewritten: skipping here leaves `local v` and
-- `v = getTargets(data)` apart, and the bytecode oracle sees `Circle` and
-- `Full` differ (their baseline is exact).
-- Negative: `Mixed` spells that shape by hand with `"Units"`: not a copy of
-- `getTargets`, it stays as written around its rebuilt `getEnemies` call.
local isServer = select("#", ...) > 5
local Nodes = {
	GET_ENEMY_CFRAMES = { InvokeSelf = function(_, data) return { "enemy-" .. tostring(data.GameID) } end },
	GET_ALL_UNITS = {
		InvokeSelf = function(_, id)
			return { { GetCFrame = function() return "unit-" .. tostring(id) end } }
		end,
	},
}
local tagged = { "fallback" }

local function getEnemies(p)
	return Nodes.GET_ENEMY_CFRAMES:InvokeSelf(p)
end

local function getUnits(data)
	if not isServer then
		tagged[#tagged + 1] = data.TargetType
		return { tagged[1], data.Range }
	end
	local units = Nodes.GET_ALL_UNITS:InvokeSelf(data.GameID)
	local cframes = {}
	for k, unit in units do
		cframes[k] = unit:GetCFrame()
	end
	return cframes
end

local function getTargets(data)
	if data.TargetType == "Unit" then
		return (getUnits(data))
	end
	return getEnemies(data)
end

local function describe(targets, label)
	local parts = {}
	for _, target in targets do
		parts[#parts + 1] = tostring(target)
	end
	return label .. ":" .. table.concat(parts, ",")
end

local Hitbox = {}

function Hitbox.Circle(data)
	local size = data.HitboxSize
	local gameID = data.GameID
	local targets = getTargets(data)
	print("circle", size)
	return describe(targets, tostring(size) .. "@" .. tostring(gameID))
end

function Hitbox.Full(data)
	local range = data.Range
	local targets = getTargets(data)
	print("full", range)
	return describe(targets, tostring(range))
end

function Hitbox.Mixed(data)
	local targets
	if data.TargetType == "Units" then
		targets = getUnits(data)
	else
		targets = getEnemies(data)
	end
	print("mixed", #targets)
	return describe(targets, "mixed")
end

print(Hitbox.Circle({ HitboxSize = 4, GameID = 7, TargetType = "Unit", Range = 3 }))
print(Hitbox.Circle({ HitboxSize = 5, GameID = 8, TargetType = "Enemy", Range = 3 }))
print(Hitbox.Full({ Range = 9, GameID = 1, TargetType = "Enemy" }))
print(Hitbox.Full({ Range = 2, GameID = 2, TargetType = "Unit" }))
print(Hitbox.Mixed({ Range = 6, GameID = 3, TargetType = "Units" }))
print(Hitbox.Mixed({ Range = 6, GameID = 4, TargetType = "Unit" }))
print(#tagged)
