local shared = 0
local immutable = 10

local function outer(seed)
	local mutable = seed

	return function(delta)
		mutable += delta

		return function()
			shared += 1
			return immutable + mutable + shared
		end
	end
end

return outer
