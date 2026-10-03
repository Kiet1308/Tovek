-- `target(value)` and `base + value`: `target` and `base` are upvalues the
-- callback reassigns, fetched (GETUPVAL) where they stand. Folding the select
-- into the call or the sum read them before the callback ran. The pass only
-- knew the captures of the block at hand, not the module's.
local target = function(v) return "old", v end
local function callback()
    target = function(v) return "new", v end
    return 42
end
local function test(choose)
    local value
    if choose then
        value = false
    else
        value = callback()
    end
    return target(value)
end
print(test(false))
local base = 100
local function bump()
    base = 1
    return 42
end
local function arith(choose)
    local value
    if choose then
        value = 0
    else
        value = bump()
    end
    return base + value
end
print(arith(false))
