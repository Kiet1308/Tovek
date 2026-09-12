for variant = 1, 6 do
    for failure = 0, 6 do
        local trace = {}
        local events = 0
        local function event(label)
            events += 1
            table.insert(trace, label)
            if events == failure then error("STOP" .. failure, 0) end
        end
        local codec = {
            Serialize = function(value) event("serialize:" .. value[1]); return "S" .. value[1] end,
            Deserialize = function(value) event("deserialize:" .. value[1]); return "D" .. value[1] end,
            Observe = function(value) event("observe:" .. value) end
        }
        local input = if variant == 1 then "x" elseif variant == 2 then false
            elseif variant == 3 then 7 elseif variant == 4 then {"a", {"b"}}
            elseif variant == 5 then nil else {}
        local ok, result = pcall(function()
            local a = f.direct(codec, {"a"}, "plain", {"b"})
            local b = f.indirect(codec, {"a"}, false, {"b"})
            local parts = f.nouns({"one", "two"})
            return tostring(a.n) .. ":" .. a[1] .. ":" .. a[2] .. ":" .. a[3]
                .. "|" .. tostring(b.n) .. ":" .. b[1] .. ":" .. tostring(b[2]) .. ":" .. b[3]
                .. "|" .. f.describe(input) .. "|" .. table.concat(parts, ",")
                .. "|" .. #f.mutableCount(8, 1)
                .. "|" .. buffer.len(f.grow(buffer.fromstring("abc"), variant + 3))
        end)
        print(variant .. ":" .. failure .. "|" .. tostring(ok) .. ":"
            .. (if ok then result else tostring(result):match("STOP%d+") or "error")
            .. "|" .. table.concat(trace, ","))
    end
end
