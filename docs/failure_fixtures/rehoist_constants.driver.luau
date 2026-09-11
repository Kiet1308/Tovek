-- The module text replaces REHOIST_MODULE inside factory; setfenv preserves
-- real global lookups instead of turning them into wrapper-local captures.
local function pack(...)
    return { n = select("#", ...), ... }
end
local function encode(value)
    if type(value) == "number" and value == 0 then
        return 1 / value < 0 and "-0" or "+0"
    elseif type(value) == "string" then
        return (value:gsub(".", function(c) return string.format("%02x", string.byte(c)) end))
    end
    return tostring(value)
end
for _, flag in { false, true } do
    for failAt = 0, 12 do
        local events = {}
        local function event(label)
            table.insert(events, label)
            if #events == failAt then error("injected", 0) end
        end
        local env = {
            flag = flag,
            argument = "argument",
            WAIT_INTERVAL = "outer-global",
            callback = function() end,
            sink = function(...) event("sink:" .. select("#", ...)) end,
            object = setmetatable({}, {
                __newindex = function(_, key, value)
                    event("store:" .. key .. ":" .. encode(value))
                end,
            }),
            task = setmetatable({}, {
                __index = function(_, key)
                    event("lookup:" .. key)
                    return function(value) event("call:" .. key .. ":" .. encode(value)) end
                end,
            }),
        }
        local factory = function()
            --[[REHOIST_MODULE]]
        end
        setfenv(factory, env)
        local f = factory()
        local result = pack(pcall(function()
            local values = pack(f())
            if type(values[1]) == "function" then return values[1]() end
            return unpack(values, 1, values.n)
        end))
        local output = {}
        for i = 1, result.n do table.insert(output, encode(result[i])) end
        print(tostring(flag) .. ":" .. failAt .. ":" .. result.n .. ":"
            .. table.concat(output, ",") .. ":" .. table.concat(events, ";"))
    end
end
