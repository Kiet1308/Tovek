return function(initial, smoothTime, maxSpeed)
	local self = {
		Current = initial,
		Velocity = if typeof(initial) == "CFrame" then CFrame.identity else initial * 0,
		SmoothTime = smoothTime,
		MaxSpeed = if maxSpeed ~= nil then maxSpeed else math.huge,
	}
	return self
end
