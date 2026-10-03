-- The constructor `{a = x, obj:GetService("X"), 0}` reads `x` before the
-- call, which writes `x` through a closure. Moving the table declaration
-- next to its SETLIST read `x` after the call (`2` instead of `1`).
local x = 1
local object = {}

function object:GetService(name)
    x = 2
    return 9
end

local t = {a = x, object:GetService("X"), 0}
print(t.a, x, t[1])
