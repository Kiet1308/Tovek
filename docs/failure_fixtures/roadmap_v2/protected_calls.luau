local function risky(x, y)
    if x == nil then
        error("no x", 0)
    end
    return x + y, x * y
end

local function handler(message)
    return "handled:" .. tostring(message)
end

local function count(...)
    return pcall(select, "#", ...)
end

return function(env, value, extra, ...)
    local log = {}
    local ok, sum, product = pcall(risky, value, extra)
    log[#log + 1] = tostring(ok) .. ":" .. tostring(sum) .. ":" .. tostring(product)
    local ok2, message = xpcall(risky, handler, nil, extra)
    log[#log + 1] = tostring(ok2) .. ":" .. tostring(message)
    local okCount, n = count(...)
    log[#log + 1] = tostring(okCount) .. ":" .. tostring(n)
    local okLookup, service = pcall(env.lookup, env, "Service")
    log[#log + 1] = tostring(okLookup) .. ":" .. tostring(service)
    local okField, field = pcall(env.read, env.data.key)
    log[#log + 1] = tostring(okField) .. ":" .. tostring(field)
    local packed = { pcall(function()
        return value, nil, extra
    end) }
    log[#log + 1] = tostring(#packed) .. ":" .. tostring(packed[1]) .. ":" .. tostring(packed[4])
    if not pcall(env.fail) then
        log[#log + 1] = "failed"
    end
    return table.concat(log, ","), select("#", pcall(risky, value, extra))
end
