-- Copies Luau -O2 made of `stillCurrent()`, a guard `if a or b or c or d
-- then return false end` and `return not (data and data.alive and not
-- data.alive[1])`. The helper's own body has the guards nested, each with
-- the same other arm; the copy joins them (`if a or b or c or d then r =
-- false else r = ... end`) and gives the negation as its truth (`(not data
-- or not data.alive or data.alive[1]) and true or false`). Each copy is
-- `stillCurrent()` again.
local Engine = {}
Engine.__index = Engine

local log = {}
local function defer(callback)
	table.insert(log, "deferred")
	callback()
end

function Engine:Run(instance, data, mode, token)
	local useToken = token ~= nil
	local generation = self._gen or 0
	local function stillCurrent(): boolean
		if self.Connection == nil or (self._gen or 0) ~= generation or not instance.Parent or not useToken and (instance.Token or 0) ~= token then
			return false
		end
		return not (data and data.alive and not data.alive[1])
	end
	if mode == "Animate" then
		local function animate()
			if not stillCurrent() then
				return
			end
			if instance.Delay > 0 then
				table.insert(log, "animate")
				defer(function()
					if not stillCurrent() then
						return
					end
					table.insert(log, "loop")
				end)
			end
		end
		animate()
	else
		defer(function()
			if not stillCurrent() then
				return
			end
			table.insert(log, "emit")
		end)
	end
	return table.concat(log, ", ")
end

local engine = setmetatable({ Connection = true }, Engine)
print(engine:Run({ Parent = true, Delay = 1 }, nil, "Animate", nil))
print(engine:Run({ Parent = true, Delay = 0, Token = 2 }, { alive = { false } }, "Emit", 2))
print(engine:Run({ Parent = false, Delay = 1 }, { alive = { true } }, "Animate", nil))
