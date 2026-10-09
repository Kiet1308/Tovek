-- An unused binding still evaluates its value (an `__index` may run), so it
-- stays, now named for what it read: a chunk header alias in its source
-- spelling (`local OnEvent = Fusion.OnEvent`), `_player` inside a function.
local reads = {}
Fusion = setmetatable({}, {
	__index = function(_, key)
		table.insert(reads, key)
		return key
	end,
})
local Children = Fusion.Children
local OnEvent = Fusion.OnEvent
local scoped = Fusion.scoped

local function inspect(data)
	local player = data.Player
	return #reads
end

print(inspect(Fusion), table.concat(reads, ","), Children)

-- `xpcall` results read as `pcall` ones do.
local function risky(n)
	if n > 1 then
		error("too big", 0)
	end
	return n * 10
end
local ok, value = xpcall(risky, function(message)
	return "handled " .. message
end, 1)
local failed, reason = xpcall(risky, function(message)
	return "handled " .. message
end, 5)
print(ok, value, failed, reason)
