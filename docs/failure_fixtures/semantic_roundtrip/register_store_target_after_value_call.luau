-- `t.value = change()` reads a register table (and key) when the store runs,
-- after the value: `change()` reassigning them redirects the store. A
-- recovered `helper(current)` would take the table before the call.
local old = {}
local new = {}
local current = old

local function change()
	current = new
	return 1
end

local function helper(p)
	p.value = change()
	print("tail")
end

current.value = change()
print("tail")
print("values", old.value, new.value)

local t = {}
local key = "old"

local function changeKey()
	key = "new"
	return 1
end

local function keyed(k)
	t[k] = changeKey()
	print("tail")
end

t[key] = changeKey()
print("tail")
print("values", t.old, t.new)

return helper, keyed
