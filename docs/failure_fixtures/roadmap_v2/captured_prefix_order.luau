return function(fetch, consume, mode)
    if mode == "base" then
        local object = {}
        local function replace()
            object = {}
        end
        local value = fetch(replace)
        object.Value = value
        return object.Value
    elseif mode == "key" then
        local key = "old"
        local object = {}
        local function replace()
            key = "new"
        end
        local value = fetch(replace)
        object[key] = value
        return object.old, object.new
    else
        local first = "old"
        local function replace()
            first = "new"
        end
        if mode == "tuple" then
            local later = fetch(replace)
            return first, later
        end
        local later = fetch(replace)
        return consume(first, later)
    end
end
