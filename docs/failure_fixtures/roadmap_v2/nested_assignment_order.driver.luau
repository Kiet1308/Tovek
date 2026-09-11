for _, shape in { "base", "key", "both", "leaf" } do
    for _, failAt in { "none", "factory", "base", "key", "invoke", "store" } do
        for _, input in { 7, false, "nil" } do
            for _, keyKind in { "string", "nil", "nan" } do
                local events = {}
                local stored = table.pack()
                local function mark(stage)
                    events[#events + 1] = stage
                    if stage == failAt then error(stage, 0) end
                end
                local function factory()
                    mark("factory")
                    return function()
                        mark("invoke")
                        if input == "nil" then return nil, 99, nil end
                        return input, 99, nil
                    end, "extra", nil
                end
                local function store(object, key, value)
                    mark("store")
                    rawset(object, key, value)
                    stored = table.pack(value)
                end
                local child = setmetatable({}, { __newindex = store })
                local target = setmetatable({}, {
                    __index = function(_, key)
                        assert(key == "Child")
                        mark("base")
                        return child
                    end,
                    __newindex = store,
                })
                local keys = setmetatable({}, {
                    __index = function(_, key)
                        assert(key == "Key")
                        mark("key")
                        if keyKind == "nil" then return nil end
                        if keyKind == "nan" then return 0 / 0 end
                        return "Value"
                    end,
                })
                local ok = pcall(f[shape], factory, target, keys)
                print(shape, failAt, input, keyKind, ok, stored.n, stored[1], table.concat(events, ","))
            end
        end
    end
end
