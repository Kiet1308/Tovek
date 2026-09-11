local subject = f
for _, asStatement in { false, true } do
    for _, mode in { "normal", "nil", "false", "fetch-error", "lookup-error" } do
        local events = {}
        local fetched = false
        local object = setmetatable({}, {
            __index = function(self, key)
                table.insert(events, "lookup:" .. key .. ":" .. tostring(fetched))
                if mode == "lookup-error" then error("lookup-failed") end
                return function(receiver, value)
                    table.insert(events, "call:" .. tostring(receiver == self) .. ":" .. tostring(value))
                    return "head", value, nil, "tail"
                end
            end,
        })
        local function fetch()
            table.insert(events, "fetch")
            fetched = true
            if mode == "fetch-error" then error("fetch-failed") end
            if mode == "nil" then return nil end
            if mode == "false" then return false end
            return 7
        end
        local result = table.pack(pcall(subject, object, fetch, asStatement))
        if result[1] then
            print(asStatement, mode, result.n - 1, result[2], result[3], result[4], result[5])
        else
            print(asStatement, mode, string.find(tostring(result[2]), "failed", 1, true) ~= nil)
        end
        print(table.concat(events, ","))
    end
end
