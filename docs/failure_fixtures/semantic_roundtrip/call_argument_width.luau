-- A call broke its arguments onto separate lines whenever it did not fit
-- the 120 columns left of the line, so a 55-character call deep in a UI
-- tree took five lines. A call's arguments now get `max(120 - column, 60)`
-- columns, and a trailing function or table hugs (`ForPairs(set,
-- function(...)`) when the line up to it fits. A call wider than its room
-- still breaks. `Vector` is global, so -O2 keeps its calls.
function Vector(x, y, z)
	return { X = x, Y = y, Z = z }
end

local function ForPairs(set, callback)
	local out = {}
	for key, value in set do
		local newKey, newValue = callback(key, value)
		out[newKey] = newValue
	end
	return out
end

local function deep(moveDirection, items)
	if moveDirection then
		for _, item in items do
			if item.enabled then
				for _ = 1, 1 do
					local flattened = Vector(moveDirection.X * item.scale, 0, moveDirection.Z * item.scale)
					local label = string.sub(item.name .. "_label", item.first, item.last)
					print(flattened.X, flattened.Y, flattened.Z, label)
				end
			end
		end
	end
end

deep({ X = 2, Z = 3 }, { { enabled = true, scale = 4, name = "first", first = 1, last = 3 } })
local doubled = ForPairs({ a = 1, b = 2 }, function(key, value)
	return key .. key, value * 2
end)
print(doubled.aa, doubled.bb)
print(string.format("%s|%s|%s", string.rep("x", 40), string.rep("y", 40), string.rep("z", 40)))
