local DEADZONE = 0.1
local trailSize = Vector2.new(1.5, 1.5)

local function toCurveSpace(t: number)
	return (1 + DEADZONE) * (2 * math.abs(t) - 1) - DEADZONE
end

local function warp(h: number)
	return (h % 1 + 1) % 1
end

local function shiftHue(h: number, s: number)
	return warp(h + (s - 0.5) * 1)
end

local function trailScale(distance: number, size: Vector2)
	return size * (math.clamp(distance - 10, 0, 90) / 90 * 1.5 + 1)
end

local function setTrailSize(n: number)
	trailSize = Vector2.new(n, n)
end

return function(part, x: number, h: number, s: number, distance: number)
	part.Curve = toCurveSpace(math.abs(x))
	part.Hue = shiftHue(h, s)
	part.Wrapped = warp(h + s)
	part.Scale = trailScale(distance, trailSize)
	setTrailSize(x)
end
