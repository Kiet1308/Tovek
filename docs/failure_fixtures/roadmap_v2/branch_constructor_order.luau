return {
    private = function(condition, make, observe, initial, key)
        local props = { Value = initial, Name = "Panel", [1] = "seed" }
        if condition() then
            props.Value = make("then")
        else
            props.Value = make("else")
        end
        props[key] = make("child")
        return observe(props)
    end,
    observed = function(condition, make, observe, initial, key)
        local props = { Value = initial, Name = "Panel", [1] = "seed" }
        if condition(props) then
            props.Value = make("then")
        else
            props.Value = make("else")
        end
        props[key] = make("child")
        return observe(props)
    end,
    captured = function(condition, make, observe, initial, key)
        local props = { Value = initial, Name = "Panel", [1] = "seed" }
        if condition(function() return props end) then
            props.Value = make("then")
        else
            props.Value = make("else")
        end
        props[key] = make("child")
        return observe(props)
    end,
    captured_initial = function(condition, make, observe, initial, key)
        local state = initial
        local props = { Value = state, Name = "Panel", [1] = "seed" }
        if condition(function() state = "mutated" end) then
            props.Selected = make("then")
        else
            props.Selected = make("else")
        end
        props[key] = make("child")
        return observe(props)
    end,
    initializer = function(condition, make, observe, initial, key)
        local props = { Value = make("initial"), Name = "Panel", [1] = "seed" }
        if condition() then
            props.Value = make("then")
        else
            props.Value = make("else")
        end
        props[key] = make("child")
        return observe(props)
    end,
    dynamic = function(condition, make, observe, initial, key)
        local props = { Value = initial, Name = "Panel", [1] = "seed" }
        if condition() then
            props[key] = make("then")
        else
            props[key] = make("else")
        end
        return observe(props)
    end,
    sequential = function(condition, make, observe, initial, key)
        local props = { Value = initial, Name = "Panel", [1] = "seed" }
        if condition() then
            props.Value = make("then")
        else
            props.Value = make("else")
        end
        if condition() then
            props.Value = make("then")
        else
            props.Value = make("else")
        end
        props[key] = make("child")
        return observe(props)
    end,
}
