for _, shape in { "selected", "nested", "snapshot", "observed", "captured", "initializer", "interleaved" } do
    for _, enabled in { false, true } do
        for _, failAt in { "none", "condition", "then", "else", "initial", "key", "factory", "tail", "consume", "store", "lookup:tail" } do
            for _, input in { 7, false, "nil" } do
                for _, keyKind in { "string", "nil", "nan" } do
                    local events = {}
                    local exposed
                    local function mark(stage)
                        events[#events + 1] = stage
                        if stage == failAt then error(stage, 0) end
                    end
                    local function value()
                        if input == "nil" then return nil end
                        return input
                    end
                    local function describe(v)
                        if type(v) == "table" then
                            local callback = if v.Handler then v.Handler() else "absent"
                            return tostring(v.Name) .. ":" .. tostring(v.Selected) .. ":" .. tostring(callback)
                        end
                        return tostring(v)
                    end
                    local receiver = setmetatable({}, {
                        __newindex = function(t, k, v)
                            mark("store")
                            rawset(t, k, v)
                        end
                    })
                    local handlers = {
                        condition = function()
                            mark("condition")
                            if exposed then mark("partial:" .. tostring(exposed()[1])) end
                            return enabled
                        end,
                        make = function(stage) mark(stage); return value(), nil, "extra" end,
                        tail = function() mark("tail"); return value(), nil, 99, nil end,
                        factory = function()
                            mark("factory")
                            return function(props) mark("construct"); return props, "extra-child" end
                        end,
                        key = function()
                            mark("key")
                            if keyKind == "string" then return "Property"
                            elseif keyKind == "nan" then return 0 / 0
                            end
                        end,
                        receiver = function() mark("receiver"); return receiver end,
                        inspect = function(t) mark("inspect:" .. tostring(t[1])); return enabled end,
                        register = function(getter) mark("register"); exposed = getter end,
                        consume = function(t)
                            mark("consume")
                            return describe(t[1]), describe(t[2]), describe(t[3]), describe(t[4]),
                                describe(t[5]), describe(t[6]), t.Snapshot, t.Header, t.Selected, nil
                        end
                    }
                    local api = setmetatable({}, { __index = function(_, name)
                        mark("lookup:" .. name)
                        return handlers[name]
                    end })
                    local result = table.pack(pcall(f[shape], api))
                    if result[1] then
                        print(shape, enabled, failAt, input, keyKind, result.n,
                            result[2], result[3], result[4], result[5], result[6], result[7],
                            result[8], result[9], result[10], result[11], table.concat(events, ","))
                    else
                        print(shape, enabled, failAt, input, keyKind, "error", table.concat(events, ","))
                    end
                end
            end
        end
    end
end
