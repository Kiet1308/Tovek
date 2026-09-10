return function(value)
    local total = 0
    for index = 1, 4 do
        total += value * index
    end
    return total
end
