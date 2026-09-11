local function helper(value, flag)
    if flag then
        return value
    end
    return nil
end
local function run(value, flag, sink, tail)
    local first = helper(value, flag)
    local second = helper(false, flag)
    return first, second
end
return {helper = helper, run = run}
