local function adjust(value, bias)
    if value < 0 then return bias end
    return value * 2 + bias
end
return function(value)
    local first = adjust(value, 3)
    local second = adjust(value + 1, 3)
    local result = 0
    for i = 1, 4 do result += value * i end
    return first + second + result
end
