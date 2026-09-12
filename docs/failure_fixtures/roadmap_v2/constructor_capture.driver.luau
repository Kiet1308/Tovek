for _, mode in { "late", "early", "self", "alias", "reassign", "keyed", "list" } do
    for variant = 1, 4 do
        for failure = 0, 7 do
            local trace = {}
            local events = 0
            local getter
            local function encode(value)
                if type(value) == "function" then return "function" end
                return tostring(value)
            end
            local function snapshot(object)
                local values = {}
                for _, key in { "A", "B", "Getter", false, 1, 2, 3 } do
                    table.insert(values, encode(rawget(object, key)))
                end
                return table.concat(values, "/")
            end
            local function event(label)
                table.insert(trace, label)
                events += 1
                if events == failure then error("STOP" .. failure, 0) end
            end
            local value = if variant == 1 then nil elseif variant == 2 then false
                elseif variant == 3 then "value" else function() end
            local function nextValue(label)
                event("value:" .. label .. ":" .. (if getter then snapshot(getter()) else "private"))
                return value, "extra"
            end
            local function publish(callback)
                getter = callback
                event("publish:" .. snapshot(getter()))
            end
            local function key()
                event("key")
                return if variant == 1 then nil elseif variant == 2 then false elseif variant == 3 then "A" else 1
            end
            local ok, result = pcall(f[mode], nextValue, publish, key)
            local message
            if ok then
                local object = result()
                message = snapshot(object)
                if object.Getter then assert(object.Getter() == object) end
            else
                message = tostring(result):match("STOP%d+") or "error"
            end
            print(mode .. ":" .. variant .. ":" .. failure .. "|" .. tostring(ok) .. ":" .. message .. "|" .. table.concat(trace, ","))
        end
    end
end
