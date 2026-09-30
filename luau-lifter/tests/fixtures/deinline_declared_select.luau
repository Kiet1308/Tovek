local function horizontal(v: Vector3)
	if v.Magnitude <= 1e-6 then
		return "flat"
	end
	return Vector3.new(v.X, 0, v.Z).Unit
end

return function(camera, root, moveDirection: Vector3)
	local DISTANCE_EPSILON = 1e-6
	local look = horizontal(camera.CFrame.LookVector)
	local right = horizontal(camera.CFrame.RightVector)
	if moveDirection.Magnitude > DISTANCE_EPSILON then
		local facing = horizontal(root.CFrame.LookVector)
		if (moveDirection - look).Magnitude <= DISTANCE_EPSILON then
			return facing, right
		end
	end
	return look, right
end
