local function show(value)
    if type(value) == "number" then
        if value ~= value then return "nan" end
        if value == 0 then return 1 / value < 0 and "-zero" or "+zero" end
        return string.format("%.17g", value)
    end
    if type(value) == "table" then
        if value.box ~= nil then return "box:" .. show(value.box) end
        return "table:" .. tostring(value.Name) .. ":" .. show(value.flag)
            .. ":" .. show(value[1]) .. ":" .. show(value[2]) .. ":" .. show(value[3]) .. ":" .. show(value[4])
    end
    return tostring(value)
end
for _, kind in ipairs({"negative", "fraction", "negative_zero", "zero", "infinity", "nan", "false", "nil", "boxed"}) do
    for _, flag in ipairs({false, true}) do
        for failAt = 0, 8 do
            local trace = {}
            local function event(text)
                table.insert(trace, text)
                if #trace == failAt then error("witness-stop", 0) end
            end
            local mt = {}
            local function raw(value) return type(value) == "table" and value.box or value end
            local function box(value) return setmetatable({box = value}, mt) end
            mt.__mul = function(a, b)
                event("mul:" .. show(a) .. ":" .. show(b))
                return box(raw(a) * raw(b))
            end
            mt.__add = function(a, b)
                event("add:" .. show(a) .. ":" .. show(b))
                return box(raw(a) + raw(b))
            end
            local values = {negative = -3, fraction = 0.25, negative_zero = -0.0, zero = 0,
                infinity = math.huge, nan = 0/0, ["false"] = false, boxed = box(2)}
            local function sink(label, value) event("sink:" .. label .. ":" .. show(value)) end
            local function tail() event("tail"); return 21, nil, 23 end
            local function pack(...) return {n = select("#", ...), ...} end
            local result = pack(pcall(f.run, values[kind], flag, sink, tail))
            local output = {}
            if result[1] then
                for index = 2, result.n do table.insert(output, show(result[index])) end
            else
                -- Compare error occurrence/order, without subject-file line numbers.
                table.insert(output, string.find(tostring(result[2]), "witness-stop", 1, true) and "stop" or "type-error")
            end
            print(kind, flag, failAt, result[1], result[1] and result.n - 1 or 0,
                table.concat(output, "|"), table.concat(trace, ","))
        end
    end
end
