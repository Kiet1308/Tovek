return {
    base = function(factory, target, keys)
        local handle = factory()
        target.Child.Value = handle()
    end,
    key = function(factory, target, keys)
        local handle = factory()
        target[keys.Key] = handle()
    end,
    both = function(factory, target, keys)
        local handle = factory()
        target.Child[keys.Key] = handle()
    end,
    leaf = function(factory, target, keys)
        local handle = factory()
        target.Value = handle()
    end,
}
