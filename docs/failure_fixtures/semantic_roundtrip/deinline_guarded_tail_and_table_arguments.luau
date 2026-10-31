-- `invokeAction` holds a copy of `setStatusWithSummary` before its `return
-- result`, which Luau's return duplicated into the copy's guard (`if
-- summary then ... return result end; ...; return result`): matched as
-- `if summary then ... else ... end` followed by the return, its body is
-- `setStatusWithSummary(context, label, result)` again. Its own copies
-- pass constructors (`{}`, `{ minutes = minutes }`) the body reads after
-- `context.invoke`: made first in the call, they read nothing that
-- changes and run nothing, and each copy is `invokeAction(...)` again.
local function summarize(result, ...)
	return type(result) == "table" and result.status or nil
end

local function setStatusWithSummary(context, label, result)
	local summary = summarize(result)
	if summary then
		context.setStatus(label .. " | " .. summary, false)
	else
		context.setStatus(label, false)
	end
end

local function invokeAction(context, action, payload, label)
	local ok, result = context.invoke(action, payload)
	if not ok then
		context.setStatus("Error: " .. tostring(result), true)
		return nil
	end
	setStatusWithSummary(context, label, result)
	return result
end

local log = {}
local context = {
	invoke = function(action, payload)
		if action == "fail" then
			return false, "unknown action"
		end
		return true, { status = action .. tostring(payload.minutes or "") }
	end,
	setStatus = function(text, failed)
		table.insert(log, text .. (failed and "!" or ""))
	end,
}

local actions = {
	fetch = function()
		invokeAction(context, "fetch", {}, "Fetched")
	end,
	set = function(minutes)
		invokeAction(context, "set", { minutes = minutes }, string.format("Set %s", tostring(minutes)))
	end,
}

for _, name in { "fetch", "set" } do
	actions[name](5)
end
print(invokeAction(context, "fail", {}, "Failed"))
print(table.concat(log, ", "))
