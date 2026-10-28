return function(value, replacement, invoke)
    local function change()
        value = replacement
        return 2
    end
    local function inner()
        return (value or value) + invoke(change)
    end
    return inner()
end
