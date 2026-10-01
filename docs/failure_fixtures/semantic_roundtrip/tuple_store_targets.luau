-- `x, y = f()` into upvalues compiles to a call into fresh registers and one
-- store per target. Printed back as one multiple assignment, the call still
-- runs once and every target takes its own result. A swap goes through one
-- temporary the same way and folds back to `a, b = b, a`.
local ready, store
local calls = 0
local function load()
	calls += 1
	return calls * 10
end
local function refresh()
	ready, store = pcall(load)
end
refresh()
refresh()
print(ready, store, calls)

local count, first, second
local function pack(...)
	return select("#", ...), ...
end
local function split(...)
	count, first, second = pack(...)
end
split("a", "b", "c")
print(count, first, second)

local left, right = 1, 2
local function swap()
	left, right = right, left
end
swap()
print(left, right)

local key, value
local function step(t)
	key, value = next(t, key)
	return key
end
local seen = {}
local items = { x = 1 }
while step(items) do
	table.insert(seen, key .. "=" .. tostring(value))
end
print(#seen, seen[1])
