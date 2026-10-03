-- `{99, [1] = 42}` drained by `p[1] = nil`: taking `[1] = 42` out of the
-- constructor left the positional `99` in slot 1, so `p[1]` read `99`.
local function f()
    local p = {99, [1] = 42}
    local c = p[1]
    p[1] = nil
    return c, p[1]
end
print(f())
