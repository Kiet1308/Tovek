-- O0 can leave a partial constructor containing [2] before its SETLIST.
-- Reserve the original array before the keyed stores; do not shift [2] to [1].
local function make(input, flip, ...)
    local b = if flip then 1 else -1
    local cell = input + 1
    local t = {
        x = b,
        ["two words"] = nil,
        [2] = if flip then input else cell,
        ["two words"] = nil,
        1, b, 0, if not flip then 10 else 256, ...
    }
    t[-0] = -0.5
    return t
end
for _, flip in {false, true} do
    for _, index in {0, 8, 9} do
        local t = make(1, flip, 3, nil)
        if index > 0 then t[index] = index end
        print(#t, t[1], t[2], t[3], t[4], t[5], t[6])
        for key, value in next, t do print(type(key), key, value) end
    end
end
