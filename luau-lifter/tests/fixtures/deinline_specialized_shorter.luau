-- `helper(false)` inlines without the `if flag` branch, one statement
-- shorter than the helper's body; `leading(false)` loses its first
-- statement, which the scan keyed the helper on.
local function helper(flag)
	print("start", flag)
	if flag then
		print("enabled")
	end
	print("end")
end

local function leading(flag)
	if flag then
		print("first")
	end
	print("then", flag)
	print("last")
end

return function(x)
	helper(false)
	helper(true)
	helper(x)
	leading(false)
	leading(true)
end
