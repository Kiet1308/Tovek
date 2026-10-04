-- `[0]` and `[-0]` name one table slot. A store to it folded into the
-- constructor must replace the entry a later duplicate would overwrite, and
-- a store through the other zero is the same key, not a new one.
local function f()
	local t = { [0] = 0, [-0] = 10 }
	t[0] = 20
	return t[0]
end
print(f())

local function g(k)
	local t = { [0] = 1 }
	t[-0] = 5
	return t[0], t[k]
end
print(g(0))
