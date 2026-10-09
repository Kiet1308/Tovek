-- A `_N` the evidence spells is part of the name, not a counter: the
-- children `Wheel_2` and `Wheel_3` name `wheel_2` and `wheel_3`, the keys
-- `slot_2`, `slot_3` and `Level_10` name `slot_2`, `slot_3` and `level_10`.
-- Suffix compaction used to take any camelCase `x_N` for a counter a late
-- pass appended, and renumbered these to `wheel` and `wheel_2`, so the
-- local spelled `wheel_2` held the child `Wheel_3`. Only a counter a pass
-- records when it appends one compacts: the receiver temps of the two
-- `child:Clone().Parent = ...` stores in separate branches are counted
-- `clone` and `clone_2` function-wide, and the second one may read `clone`.
local model = {
	FindFirstChild = function(self, name)
		return name
	end,
}

local function wheels(m)
	local front = m:FindFirstChild("Wheel_2")
	local back = m:FindFirstChild("Wheel_3")
	print(front, back)
	return front, back
end

local function slots(state)
	local a = state.slot_2
	local b = state.slot_3
	local c = state.Level_10
	state.x = a
	state.y = b
	state.z = c
	return a, b, c
end

local log = {}
local Part = {}
Part.__index = Part

function Part:Clone()
	local name = self.name
	return setmetatable({}, {
		__newindex = function(_, key, value)
			table.insert(log, name .. "." .. key .. "=" .. value)
		end,
	})
end

local function copies(child, flag)
	if flag then
		child:Clone().Parent = "first"
	else
		child:Clone().Parent = "second"
	end
	child:Clone().Parent = "third"
end

print(wheels(model))
print(slots({ slot_2 = 1, slot_3 = 2, Level_10 = 3 }))
copies(setmetatable({ name = "Rock" }, Part), #log == 0)
copies(setmetatable({ name = "Leaf" }, Part), #log == 0)
print(table.concat(log, " "))
