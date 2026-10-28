return function(register)
    local cell = 10
    register(function() cell = 20 end)
    local function read()
        local saved = cell
        return saved + saved
    end
    local first = read()
    return math.max(first, 2), read
end
