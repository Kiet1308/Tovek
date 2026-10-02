-- `local b = count; local r = b + touch()`: Luau reads the register `count`
-- when `+` runs, after `touch()` wrote it, so the snapshot stays.
local count = 0
local obj = {}
function obj.touch() count = count + 10 return 1 end
local b = count
local a = obj.touch()
local r = b + a
print(r, count)
