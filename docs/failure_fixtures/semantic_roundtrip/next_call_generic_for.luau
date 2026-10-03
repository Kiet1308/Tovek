-- `for _, v in next, folder:GetChildren() do` (user reports: Dodge,
-- UIController). At -O1+ the call fills the state and control slots of
-- FORGPREP_NEXT; the SSA inliner folded such a pack only when it covered
-- every trailing slot, so the loop stayed unstructurable ("residual
-- goto/label") and the whole script failed.
local function two()
    return {7, 8}, nil
end
local function one()
    return {9}
end
local function a(...)
    for k, v in next, ... do
        print("a", k, v)
    end
end
local function b()
    for k, v in next, two() do
        print("b", k, v)
    end
    for k, v in next, one() do
        print("b1", k, v)
    end
end
local function c(obj, flag)
    local found = false
    for _, v in next, obj:list() do
        if v == flag then
            found = true
            break
        end
    end
    return found
end
local function d(obj)
    local n = 0
    for _, v in next, obj.list(obj) do
        for _, w in next, obj:list() do
            n += v * w
        end
    end
    return n
end
local function e(gen, obj)
    for k, v in gen, obj:list() do
        print("e", k, v)
    end
end
a({1, 2})
b()
local obj = {list = function() return {1, 2, 3} end}
print(c(obj, 2), c(obj, 5), d(obj))
e(next, obj)
