local function helper(value, mode)
    if mode == "double" then
        return value * 2
    end
    return value + 3
end
local function run(value, flag, sink, tail)
    local first = helper(value, "double")
    local second = helper(value, "add")
    return first, second
end
return {helper = helper, run = run}
