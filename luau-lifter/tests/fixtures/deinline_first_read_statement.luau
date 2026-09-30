local k = 0.35
local lowerK = 0.8

local function sCurve(t: number)
	t = math.clamp(t, -1, 1)
	if t >= 0 then
		return (k * t) / (k - t + 1)
	end
	return -((lowerK * -t) / (lowerK + t + 1))
end

local function toCurveSpace(t: number)
	return (1 + 0.1) * (2 * math.abs(t) - 1) - 0.1
end

local function keep(point: Vector2, part)
	part.Size = point / 2
	part.Visible = true
end

return function(axis: number, parts, part)
	local total = 0
	local start = os.clock()
	local function syncTo(frame: number)
		local waitFor = start + frame / 60 - os.clock()
		if waitFor > 0 then
			task.wait(waitFor)
		end
	end
	for _, entry in parts do
		total += entry.Frames
		syncTo(total)
		entry.Done = true
	end
	keep(Vector2.new(axis, axis), part)
	return sCurve(toCurveSpace(math.abs(axis))), function()
		total = 0
	end
end
