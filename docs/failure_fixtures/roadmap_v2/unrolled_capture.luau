return function(value, install)
    local current = value
    if install then install(function(replacement) current = replacement end) end
    return 0 + current * 1 + current * 2 + current * 3 + current * 4
end
