-- `helper(false)` inlines without the `if flag` branch, one statement
-- shorter than the helper's body.
local function helper(flag)
	print("start", flag)
	if flag then
		print("enabled")
	end
	print("end")
end

return function(x)
	helper(false)
	helper(true)
	helper(x)
end
