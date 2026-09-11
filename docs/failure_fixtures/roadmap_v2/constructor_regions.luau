return {
    selected = function(api)
        return api.consume({
            [1] = "old",
            if api.condition() then api.make("then") else api.make("else"),
            api.tail()
        })
    end,
    nested = function(api)
        return api.consume({
            api.make("prefix"),
            api.factory()({
                Name = "Panel",
                Selected = if api.condition() then api.make("then") else api.make("else"),
                [api.key()] = api.make("property"),
                Handler = function() return api.make("callback") end
            }),
            api.tail()
        })
    end,
    snapshot = function(api)
        local state = "initial"
        local function change()
            state = "changed"
            return api.condition()
        end
        return api.consume({
            Snapshot = state,
            if change() then api.make("then") else api.make("else"),
            api.tail()
        })
    end,
    observed = function(api)
        local result = { [1] = "old" }
        local selected
        if api.inspect(result) then
            selected = api.make("then")
        else
            selected = api.make("else")
        end
        result[1] = selected
        result[2] = api.make("property")
        return api.consume(result)
    end,
    captured = function(api)
        local result = { [1] = "old" }
        api.register(function() return result end)
        local selected
        if api.condition() then
            selected = api.make("then")
        else
            selected = api.make("else")
        end
        result[1] = selected
        result[2] = api.make("property")
        return api.consume(result)
    end,
    initializer = function(api)
        return api.consume({
            Snapshot = api.make("initial"),
            [api.key()] = api.make("key-value"),
            if api.condition() then api.make("then") else api.make("else"),
            api.tail()
        })
    end,
    interleaved = function(api)
        local result = { Header = "fixed" }
        if api.condition() then
            api.receiver()[api.key()] = api.make("then")
        else
            api.receiver()[api.key()] = api.make("else")
        end
        result.Selected = api.make("property")
        return api.consume(result)
    end,
}
