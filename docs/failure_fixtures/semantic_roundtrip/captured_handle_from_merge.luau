-- A captured cell whose `nil` declaration is itself merged by an earlier branch
-- (a phi), then passes through a degenerate phi before its capture. Keeping the
-- cell must rename that phi value too; otherwise the handle is never assigned.
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

local function watch(flag, delay, limit)
	local handle
	if flag then
		handle = nil
		log[#log + 1] = "flag"
	end
	local wait = delay or 0
	local count = 0
	local function step()
		count += 1
		if count > wait and count >= limit and handle then
			handle:Disconnect()
			handle = nil
		end
	end
	handle = signal:Connect(step)
end

watch(true, 1, 3)
for _ = 1, 5 do
	signal:Fire()
end
print(table.concat(log, ","))
