return function(fetch)
    local callback = function(value) return "old", value end
    local function replaceCallback()
        callback = function(value) return "new", value end
    end
    local argument = fetch(replaceCallback)
    return callback(argument)
end
