local function weightedSum(value)
    return 0 + value * 1 + value * 2 + value * 3 + value * 4
end
return function(value)
    local first = weightedSum(value)
    local second = weightedSum(value)
    return first, second
end
