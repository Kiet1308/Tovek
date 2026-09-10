return function(value)
    local callbacks = {}
    do
        local value = value + 1
        callbacks[1] = function() return value end
    end
    do
        local value = value * 2
        callbacks[2] = function() return value end
    end
    return callbacks, value
end
