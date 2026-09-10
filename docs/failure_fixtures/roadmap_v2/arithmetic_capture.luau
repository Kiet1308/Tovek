local function weighted(value, pivot, bias)
    if value < pivot then return bias end
    return value * 2 + bias
end

return function(value, pivot, install)
    local current = value
    install(function(replacement) current = replacement end)
    return current < pivot and 3 or current * 2 + 3
end
