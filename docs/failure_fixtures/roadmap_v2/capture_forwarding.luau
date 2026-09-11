return function(fetch, mutable)
    if mutable then
        local callback = function(value) return "old", value end
        local function parent()
            local function replace()
                callback = function(value) return "new", value end
            end
            return function()
                local argument = fetch(replace)
                return callback(argument)
            end
        end
        return parent()
    end
    local callback = function(value) return "stable", value end
    local function parent()
        return function()
            local argument = fetch(function() end)
            return callback(argument)
        end
    end
    return parent()
end
