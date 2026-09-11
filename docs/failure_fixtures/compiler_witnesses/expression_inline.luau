local function helper(value)
    return (value * 2 + 3) * 4
end
local function run(value, flag, sink, tail)
    local first = helper(value)
    local second = helper(value + 1)
    return first, second
end
return {helper = helper, run = run}
