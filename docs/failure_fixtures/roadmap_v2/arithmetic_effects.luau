local function weighted(value, pivot, bias)
    if value < pivot then return bias end
    return value * 2 + bias
end

return function(value, pivot)
    local first = weighted(value, pivot, 3)
    local second = weighted(value, pivot, 3)
    return first, second
end
