-- `{[key] = 1, value}` raises for a `nil` or NaN key only after `value` was
-- computed, so the call on one branch has run by then. Folding the branch
-- into the constructor must not move the call past that error.
local calls = 0

local function change()
	calls += 1
	return 42
end

local function test(choose, key)
	local value
	if choose then
		value = false
	else
		value = change()
	end
	return { [key] = 1, value }
end

local ok = pcall(test, false, nil)
print(ok, calls)
local ok2 = pcall(test, false, 0 / 0)
print(ok2, calls)
