-- A helper's `return` from inside a loop, inlined where the caller goes on
-- after the call, is lowered to a flag and a `break`. Rebuilding the call
-- turns the flag back into the `return`; a lookalike that still reads its
-- flag afterwards must keep it.
local items = {}
local ready = false
local log = {}

local function check()
	if ready then
		return
	end
	for _, item in pairs(items) do
		if not item.Instance then
			return
		end
	end
	ready = true
	table.insert(log, "ready")
end

local function observe(child)
	check()
	table.insert(log, "observed " .. tostring(child))
end

local function scan()
	local ok = true
	for _, item in pairs(items) do
		if not item.Instance then
			ok = false
			break
		end
	end
	if ok then
		table.insert(log, "all")
	end
	return ok
end

items = { { Instance = 1 }, { Instance = false } }
observe(1)
print(scan())
items[2].Instance = 2
observe(2)
print(scan())
print(table.concat(log, ", "))
