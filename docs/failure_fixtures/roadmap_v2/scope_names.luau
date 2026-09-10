return function(items, scale)
    local runningTotal = 0
    for index, item in items do
        local weightedScore = item.Score * scale
        runningTotal += weightedScore
        item.LastIndex = index
    end
    local finalScore = runningTotal / #items
    return finalScore
end
