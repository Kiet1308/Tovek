-- `local y = x` copies a captured cell no closure writes; destruction joins
-- the copy to the cell. The copy's last update `y += 1` is dead, and joining
-- that dead write to the copy's class made it a store into the cell, which
-- the closure then read (`3` instead of `2`).
local x = 1
local f = function() return x end

x = 2
local y = x
print(y)
y += 1
print(f())
