-- A call that may write a captured cell must stay before a later read of
-- that cell. At -O2 Luau inlines `hash`, leaving `local v3 = fn_1()` above
-- `value = value + (v3 and 1 or 0)`; the call must not move into the sum.
local value = 0
local function hash(input, type_id)
	if type_id == 1 then
		value = value + input
	elseif type_id == 2 then
		value = value + (input and 1 or 0)
	end
	return input
end

function fn_1()
	hash(1, 1)
	return false, false
end

local v2 = (function(...)
	local v3 = fn_1()
	hash(v3, 2)
	return false, 0, ""
end)()

local count = 0
local function touch()
	count += 10
	return 1
end
local function mixed()
	local a = touch()
	local b = count
	return a + b
end
print(value, v2, mixed(), count)
