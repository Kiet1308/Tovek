local TweenService = game:GetService("TweenService")
local camera = workspace.CurrentCamera

local function tween(instance, info, goal)
	TweenService:Create(instance, info, goal):Play()
end

-- Returns two of its own closures: inlined as the body alone.
local function makeTrack(instance)
	local total = 0

	local function set(items)
		for k, item in items do
			instance[k] = item
		end
	end

	local function key(frames, goal)
		tween(instance, TweenInfo.new(frames / 60), goal)
		total += frames
		task.wait(total / 60)
	end

	return set, key
end

-- `count` is captured by `bump`: a call returns a snapshot of it.
local function counter()
	local count = 0
	local function bump()
		count += 1
		return count
	end
	return bump, count
end

-- Hands back its argument as a second result.
local function toNames(list)
	local names = {}
	for i = 1, #list do
		names[i] = list[i].Name .. "!"
	end
	return names, list
end

-- Returns one closure, which only a prototype tells apart.
local function FrameClock()
	local last = 0
	return function(frame)
		local wait = frame - last
		if wait > 0 then
			task.wait(wait / 60)
			last = frame
		end
	end
end

return function(lighting, effect)
	task.spawn(function()
		local set, key = makeTrack(camera)
		set({ FieldOfView = 40 })
		key(22, { FieldOfView = 50 })
	end)
	local setLighting, keyLighting = makeTrack(lighting)
	setLighting({ Brightness = 2 })
	keyLighting(10, { Brightness = 1 })
	local setEffect, keyEffect = makeTrack(effect)
	keyEffect(3, { Size = 4 })
	local bump, count = counter()
	print(bump(), count)
	local clock = FrameClock()
	clock(30)
	clock(60)
	local names, items = toNames(effect:GetChildren())
	print(names, items)
end
