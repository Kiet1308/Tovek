return function(seed, record)
    local initial = seed
    do
        local reused = seed + 1
        record("first", reused)
    end
    do
        local reused = seed * 3
        record("second", reused)
    end
    local value = seed
    local first = function() return value end
    value += 1
    local snapshot = value
    local second = function() return snapshot end
    value += 2
    return first, second, initial
end
