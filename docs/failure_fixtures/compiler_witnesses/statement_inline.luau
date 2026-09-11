local function helper(sink, value)
    sink("begin", value)
    sink("end", value + 1)
end
local function run(value, flag, sink, tail)
    helper(sink, value)
    helper(sink, value + 2)
    return value
end
return {helper = helper, run = run}
