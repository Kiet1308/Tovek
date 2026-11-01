-- Line evidence for copies Luau -O2 made of `randomRange(lo, hi)` with
-- constants: three on one line (`Vector.new(randomRange(...), ...)`), each
-- opening with loads of its own constants before the first operation of
-- the helper it keeps (`lo + (hi - lo)` folded); and three inside a copy
-- of `randomVector`, whose `Vector.new(` call is written over several
-- lines, its arguments below its own line, right before another copy.
-- Counted right (one copy each, the inner ones nested), every copy is
-- `randomRange(...)` again.
local Vector = {}
function Vector.new(x, y, z)
	return { X = x, Y = y, Z = z }
end

local function randomRange(minValue: number, maxValue: number)
	return minValue + (maxValue - minValue) * math.random()
end

local function randomVector(sizeMin, sizeMax)
	return Vector.new(
		randomRange(sizeMin.X, sizeMax.X),
		randomRange(sizeMin.Y, sizeMax.Y),
		randomRange(sizeMin.Z, sizeMax.Z)
	)
end

local function spawnShard(sizeMin, sizeMax)
	return randomVector(sizeMin, sizeMax), randomRange(0.5, 1.5)
end

local function spawnBurst(spreadMin, spreadMax)
	local offset = Vector.new(randomRange(-0.08, 0.08), randomRange(0, 0.08), randomRange(-0.08, 0.08))
	local spin = Vector.new(randomRange(-math.pi, math.pi), randomRange(-math.pi, math.pi), 0)
	local angle = randomRange(0, math.pi * 2)
	return offset.X * randomRange(spreadMin, spreadMax), offset.Y, offset.Z, spin.X, spin.Y, angle
end

math.randomseed(7)
print(spawnBurst(1, 2))
local size, scale = spawnShard(Vector.new(1, 1, 1), Vector.new(2, 3, 4))
print(size.X, size.Y, size.Z, scale)
