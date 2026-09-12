local modes = { "direct", "nested", "intervening", "captured", "shared", "constructor", "callable" }
local parentEnvironment = getfenv()
for _, mode in modes do
    for variant = 1, 4 do
        for failure = 0, 5 do
            local trace = {}
            local events = 0
            local function event(label)
                table.insert(trace, label)
                events += 1
                if events == failure then error("STOP" .. failure, 0) end
            end
            local widget = if variant == 1 then nil elseif variant == 2 then false elseif variant == 3 then "widget" else function(value) return value + 1, false end
            local function encode(value)
                if type(value) == "function" then return "function" end
                return tostring(value)
            end
            local function receiver(label)
                return setmetatable({}, {
                    __newindex = function(object, key, value)
                        event("store:" .. label .. ":" .. key .. ":" .. encode(value))
                        rawset(object, key, value)
                    end,
                    __index = function(object, key)
                        event("get:" .. label .. ":" .. key)
                        if key == "Nested" then return object end
                        return nil
                    end,
                })
            end
            local initial = receiver("initial")
            local replacement = receiver("replacement")
            local setter
            local function bind(callback) event("bind"); setter = callback end
            local function step() event("step") end
            local environment = setmetatable({ require = function(name)
                event("require:" .. name)
                if setter then setter(replacement) end
                return widget, "extra"
            end }, { __index = parentEnvironment })
            local run = f[mode]
            setfenv(run, environment)
            local result = table.pack(pcall(run, initial, if mode == "captured" then bind else step))
            local values = {}
            for index = 1, result.n do
                local value = result[index]
                if index == 2 and not result[1] then
                    value = tostring(value):match("STOP%d+") or "error"
                elseif type(value) == "table" then
                    local label = if value == initial then "initial" elseif value == replacement then "replacement" else "constructed"
                    value = label .. ":" .. encode(rawget(value, "Widget"))
                else
                    value = encode(value)
                end
                table.insert(values, value)
            end
            print(mode .. ":" .. variant .. ":" .. failure .. "|" .. table.concat(values, ",") .. "|" .. table.concat(trace, ","))
        end
    end
end
