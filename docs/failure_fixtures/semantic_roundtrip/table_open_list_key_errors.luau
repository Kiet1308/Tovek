-- Check each initial key before later prefix effects, then keep native list growth.
local events = {}
local function mark(label, value)
    events[#events + 1] = label
    if value == "error" then error(label) end
    return value
end
local function tail(...)
    mark("tail", nil)
    return ...
end
local function make(key, value, flag, ...)
    return {
        [mark("key", key)] = mark("first", value),
        second = mark("second", nil),
        [key] = mark("repeat", value),
        if flag then mark("yes", 11) else mark("no", 12),
        tail(...),
    }
end
local function run(key, value, flag)
    events = {}
    local ok, result = pcall(make, key, value, flag, 5, nil, 7)
    print(ok, table.concat(events, ","))
    if ok then
        result[8] = 8
        print("length", #result)
        for k, v in next, result do print(type(k), k, v) end
    else
        print("error", string.match(result, ":%d+: (.*)") or result)
    end
end
run("extra", 9, true)
run(false, nil, false)
run(2, false, true)
run(nil, 9, true)
run(0 / 0, 9, false)
run("extra", "error", true)
