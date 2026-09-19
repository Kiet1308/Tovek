local M = {}

function M.length(api, input)
    local count = #input
    local nextCount = count + 1
    local half = count // 2
    local remainder = count % 2
    local square = count ^ 2
    local zeroDivisor = count // 0
    api.between()
    return api.consume(nextCount, half, remainder, square, zeroDivisor)
end

function M.changed(api, input)
    local count = #input
    local nextCount = count + 1
    api.between(function(value) count = value end)
    return api.consume(nextCount, count)
end

function M.recursive(api)
    local recurse
    recurse = function(depth)
        local saved = recurse
        api.between()
        if depth == 0 then return "end", nil end
        return saved(depth - 1)
    end
    return recurse
end

function M.reassigned(api)
    local recurse
    recurse = function(depth)
        local saved = recurse
        api.between(function(value) recurse = value end)
        if depth == 0 then return "old" end
        return saved(depth - 1)
    end
    return recurse
end

function M.late(api)
    local helper
    api.capture(function() return helper end)
    helper = function() return "installed" end
    return helper
end

function M.conditional(api, enabled)
    local helper
    if enabled then helper = function() return "yes" end
    else helper = function() return "no" end end
    local saved = helper
    api.between()
    return saved()
end

function M.loop()
    local result = {}
    for i = 1, 3 do
        local helper
        helper = function(depth)
            if depth == 0 then return i end
            return helper(depth - 1)
        end
        result[i] = helper
    end
    return result
end

return M
