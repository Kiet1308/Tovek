return function(object, fetch, asStatement)
    if asStatement then
        local argument = fetch()
        object:consume(argument)
        return "statement"
    end
    local argument = fetch()
    return object:consume(argument)
end
