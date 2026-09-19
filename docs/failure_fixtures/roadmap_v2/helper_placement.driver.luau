local function values(...)
    local result = {tostring(select("#", ...))}
    for i = 1, select("#", ...) do
        local value = select(i, ...)
        table.insert(result, if type(value) == "table" or type(value) == "function" then type(value) else tostring(value))
    end
    return table.concat(result, ":")
end
for variant = 1, 6 do
    for failure = 0, 10 do
        local trace, output = {}, {}
        local events = 0
        local function event(label, value)
            events += 1
            local text = if type(value) == "table" or type(value) == "function" then type(value) else tostring(value)
            table.insert(trace, label .. ":" .. text)
            if events == failure then error("STOP" .. failure, 0) end
        end
        local input = if variant == 1 then nil elseif variant == 2 then false
            elseif variant == 3 then 0 elseif variant == 4 then "x"
            elseif variant == 5 then {} else true
        local api = {
            event = event,
            observe = function(setter) event("observe", variant); setter(variant + 20); return setter end
        }
        local ok, err = pcall(function()
            local a, b = f.make(api), f.make(api)
            table.insert(output, values(a.fire == a.fire, a.fire == b.fire))
            table.insert(output, values(a:fire(input, nil, "tail")))
            a:set(input)
            table.insert(output, values(a:fire(input)))
            table.insert(output, values(b:fire(false, nil)))
            local observed = f.observed(api)
            table.insert(output, values(observed.fire()))
            observed.observer(31)
            table.insert(output, values(observed.fire()))
            local repeated = f.repeated()
            table.insert(output, values(repeated[1].fire == repeated[3].fire,
                repeated[1].fire(), repeated[2].fire(), repeated[3].fire()))
            local individual = f.perIteration()
            table.insert(output, values(individual[1].fire == individual[3].fire,
                individual[1].fire(), individual[2].fire(), individual[3].fire()))
            local aliases = f.aliases()
            table.insert(output, values(aliases.fire == aliases.same, aliases.fire(), aliases.same()))
            table.insert(output, values(f.recursive().fire(variant)))
            local conditional = f.conditional(input)
            table.insert(output, values(conditional and conditional.fire()))
            for _, key in {"ok", false, 0 / 0} do
                local success, item = pcall(f.invalidKey, api, key)
                table.insert(output, values(success, if success then item.fire() else tostring(item):match("STOP%d+") or "key-error"))
            end
            local success, item = pcall(f.invalidKey, api, nil)
            table.insert(output, values(success, if success then item.fire() else tostring(item):match("STOP%d+") or "key-error"))
        end)
        print(variant .. ":" .. failure .. "|" .. tostring(ok)
            .. "|" .. (if ok then "ok" else tostring(err):match("STOP%d+") or "error")
            .. "|" .. table.concat(output, ",") .. "|" .. table.concat(trace, ","))
    end
end
