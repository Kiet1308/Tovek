-- Split and repeated prefix stores must retain their native open list tail.
local events = {}
local function mark(value)
    events[#events + 1] = tostring(value)
    return value
end
local function inspect(t)
    t[8] = 8
    print(#t, table.concat(events, ","))
    for k, v in next, t do print(type(k), k, v) end
end
local function repeated(flag, ...)
    return { k = flag, ["two words"] = nil, x = mark(9), x = nil,
        if flag then mark(1) else mark(2), ... }
end
local function emptyPrefix(flag, ...)
    return { x = if flag then mark(3) else mark(4), [1] = false, [2] = nil,
        flag, 6, ... }
end
for _, flag in {false, true} do
    events = {}
    inspect(repeated(flag, 2, 3, 4, 5, 6))
    events = {}
    inspect(emptyPrefix(flag, 7, nil, 9))
end

-- Key errors occur before subsequent callbacks even across separate stores.
local function keyed(key, flag, ...)
    return { x = if flag then mark(10) else mark(11),
        [key] = mark(12), y = mark(13), flag, ... }
end
for _, key in {"extra", 2, false} do
    events = {}
    inspect(keyed(key, true, 2, 3, 4, 5))
end
for _, key in {0 / 0} do
    events = {}
    local ok = pcall(keyed, key, false, 2, 3)
    print(ok, table.concat(events, ","))
end
events = {}
local ok = pcall(keyed, nil, true, 2, 3)
print(ok)
print(table.concat(events, ","))
