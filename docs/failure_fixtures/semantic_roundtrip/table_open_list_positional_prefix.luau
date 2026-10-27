-- Mixed prefixes keep list numbering, key snapshots and single-result values.
local events = {}
local shared = { tag = "same" }
local function many(label, value)
    events[#events + 1] = label
    return value, "extra"
end
local function tail(...)
    many("tail", nil)
    return ...
end
local function capturedKey(key, flag, many, tail, ...)
    local initialKey = key
    local function change()
        key = "changed"
        return many("changed", 11)
    end
    return {
        many("first", shared),
        [key] = 9,
        [initialKey] = many("repeat", shared),
        if flag then change() else many("no", 12),
        tail(...),
    }
end
local function keyFirst(key, flag, many, tail, ...)
    return {
        [key] = many("keyed", nil),
        many("first", shared),
        after = many("later", shared),
        if flag then many("yes", 11) else many("no", 12),
        ...,
    }
end
local function run(label, make, key, flag)
    events = {}
    local ok, result = pcall(make, key, flag, many, tail, 5, nil, shared)
    print(label, ok, table.concat(events, ","))
    if ok then
        result[8] = 8
        print("length", #result, "identity", result[1] == shared, result[5] == shared, result.before == shared)
        for k, v in next, result do
            print(type(k), k, if type(v) == "table" then v.tag else v)
        end
    else
        print(string.match(result, ":%d+: (.*)$") or result)
    end
end
for _, key in {1, 2, 3, "before", false} do
    run("captured", capturedKey, key, true)
    run("key-first", keyFirst, key, false)
end
run("captured-nil", capturedKey, nil, false)
run("first-nil", keyFirst, nil, true)
run("captured-nan", capturedKey, 0 / 0, true)
run("first-nan", keyFirst, 0 / 0, false)
