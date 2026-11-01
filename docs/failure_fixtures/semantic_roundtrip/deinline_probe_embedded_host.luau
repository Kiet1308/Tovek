-- `AverageAngle` holds two copies Luau -O2 made of `NormalizeAngle`, the
-- second opening with the statement the first one's value is embedded in
-- (`local mid = (a + diff / 2 + 4pi) % 2pi`). Line evidence counts both;
-- the probe counting matches before anything is rewritten went past that
-- statement and found one, so neither was a call. Counted from the
-- statement it is embedded in, the body is one `return` again.
local function NormalizeAngle(angle)
	angle = (angle + math.pi * 4) % (math.pi * 2)
	if angle > math.pi then
		angle = angle - math.pi * 2
	end
	return angle
end

local function AverageAngle(angleA, angleB)
	local difference = NormalizeAngle(angleB - angleA)
	return NormalizeAngle(angleA + difference / 2)
end

print(AverageAngle(1, 2), AverageAngle(-3, 3), NormalizeAngle(7))
