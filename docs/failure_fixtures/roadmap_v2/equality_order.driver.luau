local subject = f
for _, different in { false, true } do
    for _, throws in { false, true } do
        local events = {}
        local mt = {
            __eq = function(left, right)
                table.insert(events, "eq:" .. left.label .. ":" .. right.label)
                if throws and left.label == "left" then error("ordered-equality") end
                return left.label == "left"
            end,
        }
        local function fetch(label)
            table.insert(events, "fetch:" .. label)
            return setmetatable({ label = label }, mt)
        end
        local ok, value = pcall(subject, fetch, different)
        print(different, throws, ok, if ok then value else string.find(tostring(value), "ordered-equality", 1, true) ~= nil)
        print(table.concat(events, ","))
    end
end
