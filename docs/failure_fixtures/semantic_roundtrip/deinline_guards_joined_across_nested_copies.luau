-- Guards SSA joins across a copy inside a copy: `addContentId` checks its
-- id, then the copy of `addTarget` inside it checks `id == nil or
-- seen[id]`, and the caller's copies read `if id and id ~= nil and not
-- seen[id] then`. The inner guards with their block are the inner call,
-- inside an `if` of the outer one's, and then the outer call.
function normalize(value)
	if type(value) == "number" then
		return "rbxassetid://" .. value
	end
	return type(value) == "string" and value ~= "" and value or nil
end

local function addTarget(state, target)
	if target == nil or state.seen[target] then
		return false
	end
	state.seen[target] = true
	table.insert(state.targets, target)
	return true
end

local function addContentId(state, value)
	local id = normalize(value)
	if not id then
		return false
	end
	return addTarget(state, id)
end

function collect(extra)
	local state = { seen = {}, targets = {} }
	addContentId(state, "rbxassetid://1")
	addContentId(state, 2)
	addContentId(state, "rbxassetid://1")
	addContentId(state, extra)
	return #state.targets, state.targets[1], state.targets[2]
end

print(collect(""))
print(collect(3))
