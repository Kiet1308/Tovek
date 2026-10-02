-- A script table's field (`t.x`) may run `__index` writing a captured cell,
-- so it may not move past a read of that cell.
local cell = 0
t = setmetatable({}, {__index = function() cell = cell + 100 return 5 end})
local a = t.x
local y = cell
local s = a + 1
print(s, y, y)
