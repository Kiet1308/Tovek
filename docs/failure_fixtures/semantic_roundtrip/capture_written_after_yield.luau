-- A captured local written only before its capture keeps one value, so a
-- read of it may move past a call. Here the declaring function writes it
-- after a yield: a call that resumes the coroutine changes it, so `before`
-- must stay a snapshot taken before `coroutine.resume`.
local co
local read
co = coroutine.create(function(flag)
	local value
	if flag then
		value = 1
	else
		value = 3
	end
	read = function()
		local before = value
		coroutine.resume(co)
		return before, value
	end
	coroutine.yield()
	value = value + 10
end)
coroutine.resume(co, true)
print(read())

-- Settled before its capture: the snapshot may fold into the call.
local function settled(flag)
	local value
	if flag then
		value = 2
	else
		value = 4
	end
	local function show()
		return value * 2
	end
	local snapshot = value
	return show(), snapshot
end
print(settled(true), settled(false))
