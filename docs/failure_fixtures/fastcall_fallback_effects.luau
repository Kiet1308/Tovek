return function(register)
    local value = 10
    local function change() value = 20 end
    register(change)
    local function argument()
        print("argument", value)
        return value
    end
    local function inner() return math.max(argument(), 2) end
    return inner()
end
