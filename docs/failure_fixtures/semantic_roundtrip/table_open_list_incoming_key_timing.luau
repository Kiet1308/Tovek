-- Incoming captured keys must be read before value effects, including after helper inlining.
local function invoke(callback, ...) return callback(...) end
local function tail() print("tail"); return 21, nil, 23 end
local function marker() print("later"); return 7 end
local function factory(key, after, throws)
    return function(flag, invoke, tail)
    local function change()
        print("change", key, after)
        key = after
        if throws then error("callback") end
        return 9, 99
    end
    return { [key] = invoke(change), later = invoke(marker), if flag then invoke(marker) else invoke(change), tail() }
    end
end
local function build(key, after, throws, flag, invoke, tail)
    local inner = factory(key, after, throws)
    return invoke(inner, flag, invoke, tail)
end

local function run(label, before, after, throws, flag)
    print("case", label)
    local ok, result = pcall(build, before, after, throws, flag, invoke, tail)
    if ok then
        print("table", result.before, result.after, result[false], result[1], result[2], result[3], result[4], #result)
    else
        print("error", string.match(result, ":%d+: (.*)$") or result)
    end
end
run("change", "before", "after", false, true)
run("nil-to-key", nil, "after", false, false)
run("nan-to-key", 0 / 0, "after", false, true)
run("key-to-nil", "before", nil, false, true)
run("key-to-nan", "before", 0 / 0, false, false)
run("key-to-bool", "before", false, false, true)
run("throw", "before", "after", true, false)
