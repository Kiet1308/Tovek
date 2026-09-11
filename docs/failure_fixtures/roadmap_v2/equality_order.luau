return function(fetch, different)
    if different then
        local right = fetch("right")
        return fetch("left") ~= right
    end
    local right = fetch("right")
    return fetch("left") == right
end
