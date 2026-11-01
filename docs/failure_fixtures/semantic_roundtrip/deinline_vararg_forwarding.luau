-- Copies Luau -O2 made of `onParameterChanged(kind, index, slot, value)`
-- in handlers forwarding their varargs (`function(...) onParameterChanged(
-- "Stats", key, ...) end`): the constant `kind` decided its branch and
-- shows nowhere else, the varargs spread into `local _, value = ...`, and
-- `slot` is read only by a branch the constant dropped. The branch taken
-- tells `kind`; the spread is `...` again, and each handler a call.
local state = { Stats = {}, Passives = {} }
local changes = {}

local function onParameterChanged(kind, index, slot, value)
	if kind == "Stats" then
		state.Stats[index] = state.Stats[index] or {}
		state.Stats[index].Value = value
	elseif kind == "Passives" then
		state.Passives[index] = state.Passives[index] or { Values = {} }
		state.Passives[index].Values[slot] = value
	end
	table.insert(changes, index)
end

local handlers = {}
for key = 1, 2 do
	handlers[key] = {
		Stat = function(...)
			onParameterChanged("Stats", key, ...)
		end,
		Passive = function(...)
			onParameterChanged("Passives", key, ...)
		end,
	}
end

handlers[1].Stat(nil, 5)
handlers[2].Passive(3, 7)
handlers[1].Passive(1, 2, "extra")
print(state.Stats[1].Value, state.Passives[2].Values[3], state.Passives[1].Values[1], table.concat(changes, ","))
