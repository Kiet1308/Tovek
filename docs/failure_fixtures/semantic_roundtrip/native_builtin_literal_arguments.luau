local probes = {
    function(flag)
        local value = -17
        if flag then value = -17 end
        return (math.abs(value))
    end,
    function(flag)
        local value = -17
        if flag then value = -17 end
        local readers = { function() return (math.abs(value)) end }
        print("captured", value)
        return readers[1]()
    end,
    function(flag)
        local left = flag and 2 or 2
        local right = flag and 3 or 3
        return (math.abs(left + right))
    end,
}
for index, probe in ipairs(probes) do
    print(index, probe(true))
end

local factories = {
    function()
        local value = 7
        return function() return (math.abs(value)) end
    end,
    function(flag)
        local value = flag and 7 or 7
        return function() return (math.abs(value)) end
    end,
}
for index, factory in ipairs(factories) do
    local first, second = factory(true), factory(true)
    print("identity", index, first == second, first(), second())
end
