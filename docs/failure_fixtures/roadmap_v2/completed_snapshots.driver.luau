local function show(...)
    local result = { tostring(select("#", ...)) }
    for i = 1, select("#", ...) do
        local value = select(i, ...)
        local text
        if type(value) == "number" then
            text = if value ~= value then "nan" elseif value == 0 then (if 1 / value < 0 then "-0" else "0") else tostring(value)
        else
            text = if type(value) == "function" or type(value) == "table" then type(value) else tostring(value)
        end
        table.insert(result, text)
    end
    return table.concat(result, ":")
end
for variant = 1, 9 do
    for failure = 0, 16 do
        local trace, output = {}, {}
        local events = 0
        local function event(name)
            events += 1; table.insert(trace, name)
            if events == failure then error("STOP" .. failure, 0) end
        end
        local returned = if variant == 1 then 0 elseif variant == 2 then 4
            elseif variant == 3 then -1 elseif variant == 4 then 0 / 0
            elseif variant == 5 then math.huge elseif variant == 6 then false
            elseif variant == 7 then "3" elseif variant == 8 then {} else -1 / math.huge
        local input = setmetatable({}, {__len = function() event("len"); return returned end})
        local api = {
            between = function(setter)
                event("between")
                if setter then setter(function() return "new" end) end
            end,
            consume = function(...) event("consume"); return ... end,
            capture = function(callback) event("capture"); table.insert(output, show(callback())) end
        }
        local function attempt(label, callback)
            local values = table.pack(pcall(callback))
            local text = if values[1] then show(table.unpack(values, 2, values.n))
                else tostring(values[2]):match("STOP%d+") or "type-error"
            table.insert(output, label .. "=" .. tostring(values[1]) .. ":" .. text)
        end
        attempt("length", function() return f.length(api, input) end)
        attempt("changed", function() return f.changed(api, input) end)
        attempt("recursive", function()
            local a, b = f.recursive(api), f.recursive(api)
            table.insert(output, show(a == a, a == b))
            return a(2)
        end)
        attempt("reassigned", function() return f.reassigned(api)(1) end)
        attempt("late", function() return f.late(api)() end)
        attempt("conditional", function() return f.conditional(api, variant % 2 == 0) end)
        local loop = f.loop()
        table.insert(output, show(loop[1] == loop[3], loop[1](1), loop[2](1), loop[3](1)))
        print(variant .. ":" .. failure .. "|" .. table.concat(output, ",") .. "|" .. table.concat(trace, ","))
    end
end
