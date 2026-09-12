local parentEnvironment = getfenv()
for _, mode in { "checked", "mutated", "captured", "typed", "division" } do
    for variant = 1, 7 do
        for failure = 0, 5 do
            local trace = {}
            local events = 0
            local function event(label)
                table.insert(trace, label)
                events += 1
                if events == failure then error("STOP" .. failure, 0) end
            end
            local function encode(value)
                if type(value) == "number" and value ~= value then return "nan" end
                return tostring(value)
            end
            local input = if variant == 1 then 1 elseif variant == 2 then "1"
                elseif variant == 3 then setmetatable({}, { __add = function() event("add"); return 7 end })
                elseif variant == 4 then false elseif variant == 5 then nil
                elseif variant == 6 then math.huge else 0 / 0
            local environment = setmetatable({ touch = function(value)
                event("touch")
                if type(value) == "function" then value("changed") end
                return "changed"
            end }, { __index = function(_, key)
                if key == "consume" then
                    event("lookup:consume")
                    return function(...)
                        local values = table.pack(...)
                        for i = 1, values.n do values[i] = encode(values[i]) end
                        event("consume:" .. table.concat(values, ",", 1, values.n))
                    end
                end
                return parentEnvironment[key]
            end })
            local run = f[mode]
            setfenv(run, environment)
            local ok, message = pcall(run, input)
            local result = if ok then "ok" else tostring(message):match("STOP%d+") or "error"
            print(mode .. ":" .. variant .. ":" .. failure .. "|" .. result .. "|" .. table.concat(trace, ","))
        end
    end
end
