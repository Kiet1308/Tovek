return {
    late = function(nextValue)
        local components = {}
        components.A = nextValue("A")
        components.B = nextValue("B")
        return function() return components end
    end,
    early = function(nextValue, publish)
        local components = {}
        local getter = function() return components end
        publish(getter)
        components.A = nextValue("A")
        components.B = nextValue("B")
        return getter
    end,
    self = function(nextValue)
        local components = {}
        components.Getter = function() return components end
        components.A = nextValue("A")
        return components.Getter
    end,
    alias = function(nextValue, publish)
        local components = {}
        local alias = components
        local getter = function() return alias end
        publish(getter)
        components.A = nextValue("A")
        components.B = nextValue("B")
        return getter
    end,
    reassign = function(nextValue)
        local components = {}
        components.A = nextValue("A")
        local getter = function() return components end
        components = { B = nextValue("replacement") }
        return getter
    end,
    keyed = function(nextValue, _, key)
        local components = {}
        components[key()] = nextValue("dynamic")
        components.A = nextValue("A")
        return function() return components end
    end,
    list = function(nextValue)
        local components = { nextValue("one"), nextValue("tail") }
        return function() return components end
    end,
}
