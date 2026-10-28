return function(register)
    local cell = 10
    register(function() print("seen", cell) end)
    local function argument()
        cell = 20
        return 1
    end
    local first = argument()
    return math.max(first, 2), argument
end
