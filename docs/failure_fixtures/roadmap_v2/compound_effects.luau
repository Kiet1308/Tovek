return function(mode, record, key: number)
    if mode == "base" then
        record.branch.value = record.branch.value + 1
    elseif mode == "key" then
        record[key + 1] = record[key + 1] + 1
    else
        record[-key] = record[-key] + 1
    end
end
