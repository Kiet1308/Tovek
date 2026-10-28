return function(register)
    local value = 10
    local function change() value = 20 end
    register(change)
    local function inner() return math.max(value, 2) end
    return inner()
end
