-- The script replaces `math`, so `math.abs` runs `__index`, which writes
-- `x`. The SSA inliner took any library path as code-free and moved the
-- snapshot `old = x` before the lookup.
local x = 1
math = setmetatable({}, {__index = function(_, key)
    x = 0
    return 7
end})
local function f()
    local y = math.abs
    local old = x
    return y, old, old
end
print(f())
