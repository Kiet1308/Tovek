-- A compound target's base is evaluated once: `proxy[i]` must reach
-- __index once per update, never twice as `proxy[i].count = proxy[i].count + step`.
return function(items, step)
    local reads = 0
    local proxy = setmetatable({}, {
        __index = function(_, key)
            reads += 1
            return items[key]
        end,
    })
    for i = 1, #items do
        proxy[i].count += step
        proxy[i].label ..= "!"
    end
    return reads, items[1].count, items[2].count, items[2].label
end
