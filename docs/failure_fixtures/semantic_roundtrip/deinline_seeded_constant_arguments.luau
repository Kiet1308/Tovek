-- A logging helper whose copies Luau folded for constant arguments: the
-- event compared down an `if` chain (`event == "CREATED"`) and a reason
-- defaulted with `or` (`reason or "N/A"`), which the copy shows as the
-- constant passed, and a local only the folded branches read (`now`),
-- which the copy keeps as its call alone. Each copy is its call with those
-- constants.
local enabled = os.clock() >= 0
local log = {}

local function logEvent(key, event, reason)
	if not enabled then
		return
	end
	local now = os.clock()
	if not log[key] then
		log[key] = { SpawnedBy = reason or "unknown", Last = event }
	end
	if event == "CREATED" then
		log[key].Created = now
	elseif event == "DESTROYED" then
		log[key].Reason = reason
	end
end

function track(key, alive)
	if alive then
		logEvent(key, "STOP", "recovery_failed")
	else
		logEvent(key, "MOVE", "distance")
	end
	return log[key].SpawnedBy, log[key].Last
end

print(track("a", true))
print(track("b", false))
print(track("a", false))
