local function make(api)
    local active = false
    local function fire(_, value, ...)
        active = not active
        api.event("fire", active)
        return value, ...
    end
    return {
        set = function(_, value) active = value; api.event("set", active) end,
        fire = fire
    }
end

local function observed(api)
    local state = 1
    local function fire() return state end
    return {
        observer = api.observe(function(value) state = value end),
        fire = fire
    }
end

local function repeated()
    local count = 0
    local function fire() count += 1; return count end
    local items = {}
    for i = 1, 3 do items[i] = {fire = fire} end
    return items
end

local function perIteration()
    local items = {}
    for i = 1, 3 do
        local snapshot = i
        local function fire() return snapshot end
        items[i] = {fire = fire}
        snapshot += 10
    end
    return items
end

local function aliases()
    local count = 0
    local function fire() count += 1; return count end
    return {fire = fire, same = fire}
end

local function recursive()
    local function fire(count)
        if count <= 0 then return 1 end
        return count * fire(count - 1)
    end
    return {fire = fire}
end

local function conditional(enabled)
    local function fire() return 17 end
    return enabled and {fire = fire}
end

local function invalidKey(api, key)
    local state = 3
    local function fire() return state end
    return {[key] = api.observe(function(value) state = value end), fire = fire}
end

return {make = make, observed = observed, repeated = repeated, perIteration = perIteration,
    aliases = aliases, recursive = recursive, conditional = conditional, invalidKey = invalidKey}
