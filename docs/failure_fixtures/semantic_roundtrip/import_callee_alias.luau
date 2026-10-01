-- Luau reads a call's callee before its arguments. Where those may run
-- code, the lifter keeps the callee in a local (`local format =
-- string.format`). A global path the script never assigns is resolved once
-- at load time, so the call can name it in place.
local log = {}

local function step(value)
	table.insert(log, value)
	return value
end

local function describe(a, b)
	return string.format("%d-%d", step(a), step(b))
end

local function joined(items)
	return table.concat({ step(items[1]), step(items[2]) }, "+")
end

print(describe(1, 2), joined({ "x", "y" }))
print(table.concat(log, ","))
