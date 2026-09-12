local function helper(value, bias)
    local product = value * 2
    return (product + bias) + 4
end

return {
    helper = helper,
    arithmetic = function(value, bias)
        local first = helper(value, bias)
        local second = helper(value, bias)
        return first, second
    end,
    field = function(api)
        local field = api.Value
        return field
    end,
    snapshot = function(api)
        local snapshot = api.Value
        api.mutate()
        return snapshot, api.Value
    end,
    callee = function(api)
        local factory = api.Factory
        return factory(api.argument())
    end,
    scalar = function(api)
        local scalar = api.tail()
        return false, scalar
    end,
    address = function(api)
        local rhs = api.value()
        api.object()[api.key()] = rhs
        return true
    end,
    guarded = function(value)
        if value == 7 then
            return value, 1 / value
        elseif value == 0 then
            return value, 1 / value
        elseif value == false then
            return value, "false"
        elseif value == nil then
            return value, "nil"
        end
        return value, "other"
    end,
    captured_guard = function(value)
        local function change()
            value = 9
        end
        if value == 7 then
            change()
            return value
        end
        return value
    end,
    cell = function(initial)
        local cell = initial
        local api = {}
        api.test = function(value)
            cell += 1
            return "ok"
        end
        local function predicate(value)
            return api.test(value) == "ok" and value > 0
        end
        local result = api.test(cell) == "ok" and cell > 0
        return result, cell, predicate
    end,
}
