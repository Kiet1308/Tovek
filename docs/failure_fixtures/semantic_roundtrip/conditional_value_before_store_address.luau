-- `object.sub[1] = value`: the store address `object.sub` is a GETTABLE
-- (here `__index`) after `value`, whose callback changes which table it
-- returns. Folding the select into the store looked the address up first.
local state = "before"
local before, after = {}, {}
local object = setmetatable({}, {__index = function(_, key)
    if state == "before" then return before end
    return after
end})
local function callback()
    state = "after"
    return 42
end
local function test(choose)
    local value
    if choose then
        value = false
    else
        value = callback()
    end
    object.sub[1] = value
end
test(false)
print(before[1], after[1])
