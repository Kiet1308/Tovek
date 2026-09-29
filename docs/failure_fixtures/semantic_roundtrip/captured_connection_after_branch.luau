-- A connection handle declared `nil`, then an unrelated branch (`delay or 0`),
-- then assigned the connection whose named callback disconnects it. The
-- branch leaves a degenerate phi between the declaration and the capture;
-- removing it must keep the captured cell, or the assignment is dropped to
-- `local _ = signal:Connect(step)` and `connection:Disconnect()` indexes nil.
-- (Real dump: a Roblox place's GuiTile:AnimSway.)
local log = {}
local signal = { handlers = {} }
function signal:Connect(fn)
	self.handlers[fn] = true
	return {
		Disconnect = function()
			log[#log + 1] = "disconnected"
			self.handlers[fn] = nil
		end,
	}
end
function signal:Fire(...)
	for fn in pairs(self.handlers) do
		fn(...)
	end
end

local function animate(limit, delay)
	local connection = nil
	local total = 0
	local wait = delay or 0
	local function step(dt)
		total += dt
		if total < wait then
			return
		end
		log[#log + 1] = "step" .. total
		if total >= limit then
			connection:Disconnect()
		end
	end
	connection = signal:Connect(step)
end

animate(3, 1)
for _ = 1, 5 do
	signal:Fire(1)
end
print(table.concat(log, ","))
