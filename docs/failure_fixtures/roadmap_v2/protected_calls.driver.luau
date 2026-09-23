local trace = {}
local function makeEnv(failRead)
    return {
        lookup = function(self, name)
            trace[#trace + 1] = "lookup:" .. name
            return name .. "!"
        end,
        read = function(key)
            trace[#trace + 1] = "read:" .. tostring(key)
            if failRead then
                error("read failed", 0)
            end
            return key
        end,
        data = setmetatable({}, {
            __index = function(_, key)
                trace[#trace + 1] = "index:" .. key
                return key .. "Value"
            end,
        }),
        fail = function()
            error("boom", 0)
        end,
    }
end
print(f(makeEnv(false), 2, 3, 1, nil, 3))
print(f(makeEnv(true), nil, 4))
print(table.concat(trace, ","))
