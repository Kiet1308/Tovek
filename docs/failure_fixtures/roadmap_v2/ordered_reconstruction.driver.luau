local function encode(value)
    if type(value) == "number" then
        if value ~= value then return "nan" end
        if value == 0 then return if 1 / value < 0 then "-0" else "+0" end
        return tostring(value)
    elseif type(value) == "table" then
        return "box:" .. encode(value.n)
    elseif type(value) == "function" then
        return "function"
    end
    return tostring(value)
end

local function observe(label, trace, run)
    local packed = table.pack(pcall(run))
    local values = {}
    for i = 1, packed.n do
        values[i] = if i == 2 and not packed[1] then tostring(packed[i]):match("E%d+") or "error" else encode(packed[i])
    end
    print(label .. "|" .. table.concat(values, ",") .. "|" .. table.concat(trace, ","))
end

for _, mode in { "field", "snapshot", "callee", "scalar", "address" } do
    for stage = 0, 10 do
        local trace = {}
        local calls = 0
        local function event(name)
            table.insert(trace, name)
            calls += 1
            if calls == stage then error("E" .. stage, 0) end
        end
        local value = 7
        local target = setmetatable({}, { __newindex = function(_, key, rhs) event("store:" .. key .. ":" .. encode(rhs)) end })
        local members = {
            mutate = function() event("mutate"); value = 9 end,
            Factory = function(argument) event("factory"); return argument, nil, false end,
            argument = function() event("argument"); return 3 end,
            tail = function() event("tail"); return nil, false, 8 end,
            value = function() event("value"); return value end,
            object = function() event("object"); return target end,
            key = function() event("key"); return "K" end,
        }
        local api = setmetatable({}, { __index = function(_, key)
            event("get:" .. key)
            return if key == "Value" then value else members[key]
        end })
        observe(mode .. ":" .. stage, trace, function() return f[mode](api) end)
    end
end

for i, value in { 7, 0, -0.0, false, "text", 0/0, math.huge, -math.huge, 2 } do
    observe("guard:" .. i, {}, function() return f.guarded(value) end)
    observe("captured:" .. i, {}, function() return f.captured_guard(value) end)
end
observe("guard:nil", {}, function() return f.guarded(nil) end)
observe("captured:nil", {}, function() return f.captured_guard(nil) end)
for initial = -2, 2 do
    observe("cell:" .. initial, {}, function() return f.cell(initial) end)
end

for _, boxed in { false, true } do
    for index, number in { 7, 0, -0.0, 0/0, math.huge, -math.huge, -2, 1e16 } do
        for stage = 0, 6 do
            local trace = {}
            local calls = 0
            local meta = {}
            local function box(n) return setmetatable({ n = n }, meta) end
            local function op(name, a, b)
                calls += 1
                table.insert(trace, name .. ":" .. encode(a) .. ":" .. encode(b))
                if calls == stage then error("E" .. stage, 0) end
                local left = if type(a) == "table" then a.n else a
                local right = if type(b) == "table" then b.n else b
                return box(if name == "mul" then left * right else left + right)
            end
            meta.__mul = function(a, b) return op("mul", a, b) end
            meta.__add = function(a, b) return op("add", a, b) end
            local value = if boxed then box(number) else number
            observe("arithmetic:" .. tostring(boxed) .. ":" .. index .. ":" .. stage, trace,
                function() return f.arithmetic(value, 3) end)
        end
    end
end
