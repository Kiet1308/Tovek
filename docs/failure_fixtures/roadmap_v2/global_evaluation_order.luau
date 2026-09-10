local function run()
    local value = fetch()
    return sink(value)
end

return function(mode)
    local trace = {}
    local env
    env = setmetatable({
        fetch = function()
            trace[#trace + 1] = "fetch"
            if mode == "nil" then
                return nil, "extra"
            elseif mode == "false" then
                return false, "extra"
            end
            return 7, "extra"
        end,
    }, {
        __index = function(_, key)
            trace[#trace + 1] = "lookup:" .. key
            if mode == "throw" then
                error("lookup failure")
            elseif mode == "mutate" then
                env.fetch = function()
                    trace[#trace + 1] = "replacement"
                    return 99
                end
            end
            return function(...)
                local values = table.pack(...)
                trace[#trace + 1] = "sink:" .. values.n .. ":" .. tostring(values[1])
                return values[1], "tail", nil
            end
        end,
    })
    setfenv(run, env)
    local results = table.pack(pcall(run))
    if not results[1] then
        return table.concat(trace, ","), "error", string.find(results[2], "lookup failure", 1, true) ~= nil
    end
    return table.concat(trace, ","), results.n, results[2], results[3], results[4]
end
