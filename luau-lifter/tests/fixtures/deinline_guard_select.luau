local function setup(signal)
	local function computeThrottle(dist)
		if dist > 0.2 then
			return 0.5 + dist ^ 2 / 2
		end
		return 0
	end

	local function steer(target, origin)
		local offset = target - origin
		return computeThrottle(math.min(1, offset.Magnitude / 50)) * math.sign(offset.Z)
	end

	signal:Connect(computeThrottle)
	signal:Connect(computeThrottle)
	return steer
end

return setup
