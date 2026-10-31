-- Copies Luau -O2 made of helpers whose selects depend on parameters: with
-- constant arguments a copy of `clamp` is `v < 0 and 0 or v > 1 and 1 or v`,
-- exact only because `0` and `1` are truthy, and its comparison reads the
-- other way round (`1 < v` printed `v > 1`). After a prefix the select
-- folds into the statement using it (`local n = tonumber(x); t.Spin = not
-- n and 0.5 or ...`).
local function clamp(value, low, high)
	if value < low then
		return low
	end
	if high < value then
		return high
	end
	return value
end

local function clampUnit(raw, low, high, default)
	local n = tonumber(raw)
	if not n then
		return default
	end
	if n < low then
		return low
	end
	if high < n then
		return high
	end
	return n
end

local Graph = {}
function Graph.blend(alpha)
	local t = clamp(alpha or 1, 0, 1)
	return t * 2
end
function Graph.fade(alpha)
	local t = clamp(alpha or 0, 0, 1)
	return 1 - t
end

function Graph.read(config)
	local out = {}
	out.Bounciness = clampUnit(config.Bounciness, 0, 1, 0.7)
	out.Spin = clampUnit(config.Spin, 0, 2, 0.5)
	return out
end

local read = Graph.read({ Bounciness = "3", Spin = "x" })
print(read.Bounciness, read.Spin, Graph.read({ Bounciness = -1 }).Bounciness)
print(Graph.blend(), Graph.blend(-1), Graph.blend(0.25), Graph.blend(3))
print(Graph.fade(), Graph.fade(0.5), Graph.fade(7))
