-- `local b = count; math.max(b, touch(), -1)`: FASTCALL reads a register
-- argument when it runs, after `touch()` wrote `count`, so the snapshot
-- stays.
local count = 0
local obj = {}
function obj.touch() count = count + 10 return 1 end
local b = count
local a = obj.touch()
local r = math.max(b, a, -1)
print(r, count)
