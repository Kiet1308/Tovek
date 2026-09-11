local function helper(value)
    return value
end
local function run(value, flag, sink, tail)
    local first = helper(value)
    local second = first
    return second, value
end
return {helper = helper, run = run}
