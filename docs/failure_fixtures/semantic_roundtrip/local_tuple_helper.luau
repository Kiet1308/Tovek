-- Helpers that return several of their own locals, inlined at -O2 as the
-- body alone with the caller's locals in place of the returned ones. The
-- rebuilt `local set, key = makeTrack(x)` must keep every call's own state,
-- and a returned local that a closure captures must not be shared.
local log = {}

local function makeTrack(target)
	local total = 0

	local function set(items)
		for k, item in items do
			target[k] = item
		end
	end

	local function key(frames, goal)
		total += frames
		set(goal)
		table.insert(log, total)
	end

	return set, key
end

local function split(value)
	local parts = string.split(value, ".")
	local part = parts[1]
	local joined
	if #parts > 1 then
		joined = table.concat(parts, ".", 2, #parts)
	else
		joined = false
	end
	return part, joined
end

-- `count` is captured by `bump`: each caller gets a snapshot, not the cell.
local function counter()
	local count = 0
	local function bump()
		count += 1
		return count
	end
	return bump, count
end

-- The returned function calls itself.
local function makeLoop(limit)
	local steps = 0
	local function loop(n)
		steps += 1
		if n < limit then
			return loop(n + 1)
		end
		return steps
	end
	return loop, limit
end

local camera = { FieldOfView = 70 }

local function run(lighting)
	local set, key = makeTrack(camera)
	set({ FieldOfView = 40 })
	key(22, { FieldOfView = 50 })
	local setLighting, keyLighting = makeTrack(lighting)
	setLighting({ Brightness = 2 })
	keyLighting(10, { Brightness = 1 })
	keyLighting(5, { Brightness = 0 })
	key(3, { FieldOfView = 60 })
	print(camera.FieldOfView, lighting.Brightness, table.concat(log, ","))

	local part, rest = split("a.b.c")
	part = part .. "!"
	print(part, rest)
	local only = split("solo")
	print(only, (select(2, split("x"))))

	local bump, count = counter()
	print(bump(), bump(), count)

	local loop, limit = makeLoop(3)
	print(loop(0), limit)
	loop = nil
	print(loop)
end

run({ Brightness = 5 })
