-- A local read by a closure inside a closure is captured by reference to the
-- parent's upvalue (`LCT_UPVAL`). Written once, that cell never changes, so a
-- helper call passing it rebuilds; one a closure rebinds stays pasted. The
-- copies Luau made of the arguments fold back into the rebuilt call.
local function remaining(target, now, base)
	if not target or target <= 0 then
		return 0
	end
	return math.max(0, math.floor(target - (now + base) + 0.5))
end

local function countdown(data)
	local base = data.base * 2
	local first = remaining(data.target, data.now, base)
	local results = { first }
	local function effect()
		table.insert(results, remaining(data.target, data.now, base))
		local function later()
			return remaining(data.target, data.now + 1, base)
		end
		table.insert(results, later())
	end
	effect()
	return results
end
local r = countdown({ target = 100, now = 10, base = 3 })
print(r[1], r[2], r[3])

local function moving(data)
	local base = 1
	local function shift()
		base += 5
	end
	local before = remaining(data.target, data.now, base)
	shift()
	local after = remaining(data.target, data.now, base)
	return before, after
end
print(moving({ target = 40, now = 2 }))
