for _, shape in { "private", "observed", "captured", "captured_initial", "initializer", "dynamic", "sequential" } do
    for _, enabled in { false, true } do
        for _, failAt in { "none", "condition", "initial", "then", "else", "child", "observe" } do
            for _, input in { 7, false, "nil" } do
                for _, keyKind in { "string", "overlap", "numeric", "nil", "nan" } do
                    local events = {}
                    local function mark(stage)
                        events[#events + 1] = stage
                        if stage == failAt then error(stage, 0) end
                    end
                    local function condition(exposed)
                        mark("condition")
                        if type(exposed) == "function" then exposed = exposed() end
                        if exposed then
                            mark("partial:" .. tostring(exposed.Value) .. ":" .. tostring(exposed.Name))
                            setmetatable(exposed, { __newindex = function(t, k, v)
                                mark("newindex:" .. tostring(k))
                                rawset(t, k, v)
                            end })
                        end
                        return enabled
                    end
                    local function make(stage)
                        mark(stage)
                        if input == "nil" then return nil, 99, nil end
                        return input, 99, nil
                    end
                    local function observe(props)
                        mark("observe")
                        return props.Name, props.Value, props.Child, props[1], nil
                    end
                    local key
                    if keyKind == "string" then key = "Child"
                    elseif keyKind == "overlap" then key = "Value"
                    elseif keyKind == "numeric" then key = 1
                    elseif keyKind == "nan" then key = 0 / 0
                    end
                    local result = table.pack(pcall(f[shape], condition, make, observe, "initial-value", key))
                    if result[1] then
                        print(shape, enabled, failAt, input, keyKind, result.n,
                            result[2], result[3], result[4], result[5], result[6], table.concat(events, ","))
                    else
                        print(shape, enabled, failAt, input, keyKind, "error", table.concat(events, ","))
                    end
                end
            end
        end
    end
end
