for variant = 1, 8 do
    for failure = 0, 10 do
        local trace = {}
        local events = 0
        local function event(name)
            events += 1
            table.insert(trace, name)
            if events == failure then error("STOP" .. failure, 0) end
        end
        local meta = {
            __eq = function() event("eq"); return variant % 2 == 0 end,
            __index = function() event("index"); return false end,
            __add = function() event("add"); return 4 end
        }
        local value = if variant == 1 then nil elseif variant == 2 then false
            elseif variant == 3 then "ready" elseif variant == 4 then 2
            elseif variant == 5 then 0 / 0 else setmetatable({}, meta)
        local other = if variant == 7 then value else setmetatable({}, meta)
        if variant == 8 then rawset(value, "field", true) end
        local packed = table.pack(pcall(f, value, other, {
            event = event,
            call = function() event("call"); return nil, false, "extra" end
        }))
        local result = if packed[1] then tostring(packed.n) .. ":" .. tostring(packed[2])
            .. ":" .. tostring(packed[3]) .. ":" .. tostring(packed[4])
            else tostring(packed[2]):match("STOP%d+") or "type-error"
        print(variant .. ":" .. failure .. "|" .. tostring(packed[1]) .. "|" .. result .. "|" .. table.concat(trace, ","))
    end
end
