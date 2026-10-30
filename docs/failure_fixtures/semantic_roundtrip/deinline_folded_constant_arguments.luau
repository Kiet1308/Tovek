-- Luau -O2 inlines a helper called with constant arguments and folds them:
-- branches on them disappear (`if a then ... end` with `a = 1`), arithmetic
-- and comparisons on them become constants (`2 * n` is `6`, `n >= 4` drops
-- its arm), and stores the arms kept fold into the constructor. The copies
-- stayed inlined; each is the helper's call again.
local function collect(a, b, c)
	local list = {}
	if a then
		list[1] = a
	end
	if b then
		list[2] = b
	end
	print(c)
	list[3] = c
	return list
end

local function fill(n)
	local out = {}
	if n >= 1 then
		out[1] = 1 * n
	end
	if n >= 2 then
		out[2] = 2 * n
	end
	if n >= 3 then
		out[3] = 3 * n
	end
	if n >= 4 then
		out[4] = 4 * n
	end
	return out
end

local function counted(v)
	local t = {}
	if v then
		t[1] = v
		t[1] += 1
		t[2] = v
	end
	return t
end

-- A value set only when given, on an object made from a class name.
local function makeProperty(className, value, callback)
	local instance = { ClassName = className, Value = false, callbacks = {} }
	if value then
		instance.Value = value
	end
	table.insert(instance.callbacks, callback)
	callback(instance.Value)
	return instance
end

local a = collect(1, nil, 3)
print(#a, a[1], a[2], a[3])
local b = collect("x", "y", "z")
for key, value in pairs(b) do
	print(key, value)
end
local f = fill(3)
print(#f, f[1], f[2], f[3], f[4])
local c = counted(4)
print(#c, c[1], c[2])
local zero = makeProperty("NumberValue", 0, function(value)
	print("zero", value)
end)
local unset = makeProperty("NumberValue", nil, function(value)
	print("unset", value)
end)
print(zero.Value, unset.Value, #zero.callbacks)
