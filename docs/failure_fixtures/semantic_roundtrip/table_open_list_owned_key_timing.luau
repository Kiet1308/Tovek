-- Captured-key timing probe; callbacks are parameters to keep the constructor's allocation visible at O2.
local function invoke(callback, ...) return callback(...) end
local function tail() print("tail"); return 21, nil, 23 end
local function marker() print("later"); return 7 end
local function ownerKey(key, after, throws, flag, invoke, tail)
    local function change()
        print("change", key, after)
        key = after
        if throws then error("callback") end
        return 9, 99
    end
    return { [key] = invoke(change), later = invoke(marker), if flag then invoke(marker) else invoke(change), tail() }
end

local function localIndexKey(before, after, throws, flag, invoke, tail)
    local key = before
    local function change()
        print("change", key, after)
        key = after
        if throws then error("callback") end
        return 9, 99
    end
    local object = setmetatable({}, { __index = function() return change() end })
    return { first = invoke(marker), [key] = object.value, later = invoke(marker), if flag then invoke(marker) else invoke(change), tail() }
end

local function savedKey(key, after, throws, flag, invoke, tail)
    local function change()
        print("change", key, after)
        key = after
        if throws then error("callback") end
        return 9, 99
    end
    local saved = key
    return { [saved] = invoke(change), later = invoke(marker), if flag then invoke(marker) else invoke(change), tail() }
end

local function compoundKey(before, after, throws, flag, invoke, tail)
    local key = before
    local function change()
        print("change", key, after)
        key = after
        if throws then error("callback") end
        return 9, 99
    end
    return { [key or "before"] = invoke(change), later = invoke(marker), if flag then invoke(marker) else invoke(change), tail() }
end

local function run(build, label, before, after, throws, flag)
    print("case", label)
    local ok, result = pcall(build, before, after, throws, flag, invoke, tail)
    if ok then
        print("table", result.before, result.after, result[false], result[1], result[2], result[3], result[4], #result)
    else
        print("error", string.match(result, ":%d+: (.*)$") or result)
    end
end
for index, build in {ownerKey, localIndexKey, savedKey, compoundKey} do
    print("builder", index)
    run(build, "change", "before", "after", false, true)
    run(build, "nil-to-key", nil, "after", false, false)
    run(build, "nan-to-key", 0 / 0, "after", false, true)
    run(build, "key-to-nil", "before", nil, false, true)
    run(build, "key-to-nan", "before", 0 / 0, false, false)
    run(build, "key-to-bool", "before", false, false, true)
    run(build, "throw", "before", "after", true, false)
end
