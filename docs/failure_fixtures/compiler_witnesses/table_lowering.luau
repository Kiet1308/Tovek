local function run(value, flag, sink, tail)
    local result = {Name = "witness", value, tail()}
    result.flag = flag
    result[2] = nil
    return result
end
return {run = run}
