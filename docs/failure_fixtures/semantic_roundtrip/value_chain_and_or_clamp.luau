-- `num > high and high or num < low and low or num`: where `high` may be
-- falsy, Luau tests it after the comparison, and the jumps read as `if
-- high < num and high then num = high elseif num < low then num = low or
-- num end`, `num` written in place. That is the value itself: the helper
-- returns it again, and its copies, the same chain (constants folded,
-- written into the caller's local, returned, or the whole value of a new
-- local after its argument temps), are `clamp(...)` again.
local function clamp(low, high, num)
	return (num > high and high or num < low and low or num)
end

local last = 0
local function update(target, now, previous)
	local delta = target - last
	if target < 1 and last < 0.95 then
		local maxDelta = 2.8 * (now - previous)
		delta = clamp(-maxDelta, maxDelta, delta)
	end
	last = last + delta
	return clamp(0, 1, math.floor(last * 100 + 0.5) / 100)
end

local function rotate(look, rotation)
	local vertical = math.asin(look.y)
	local yTheta = clamp(-1.4 + vertical, 1.4 + vertical, rotation.y)
	return rotation.x, yTheta
end

local camera = { DefaultZoom = 12 }
local function zoom(minZoom, maxZoom, enabled)
	local current
	if enabled then
		current = clamp(minZoom, maxZoom, camera.DefaultZoom) or camera.DefaultZoom
	else
		current = camera.DefaultZoom
	end
	camera.Current = current
end

print(update(0.5, 1, 0.9), update(0.2, 2, 1.95), update(3, 4, 3))
print(rotate({ y = 0.5 }, { x = 1, y = 3 }))
zoom(1, 10, true)
print(camera.Current)
zoom(1, 10, false)
print(camera.Current)
