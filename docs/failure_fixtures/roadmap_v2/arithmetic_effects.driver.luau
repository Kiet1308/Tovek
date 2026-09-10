local function classify(value)
    if value ~= value then return "nan" end
    if value == 0 then return if 1 / value < 0 then "-zero" else "+zero" end
    return tostring(value)
end
for _, value in {-2, -0.0, 0.0, 4, math.huge, -math.huge, 0 / 0} do
    local a, b = f(value, 0)
    print(classify(value), classify(a), classify(b))
end

local trace = {}
local mt = {}
function mt.__lt(a, b)
    table.insert(trace, "lt:" .. a.value)
    a.value += 1
    return a.value < b.value
end
function mt.__mul(a, b)
    table.insert(trace, "mul:" .. a.value .. ":" .. b)
    return a.value * b
end
local function box(value) return setmetatable({value = value}, mt) end
for _, value in {-3, 1} do
    table.clear(trace)
    local a, b = f(box(value), box(0))
    print("boxed", a, b, table.concat(trace, ","))
end
table.clear(trace)
mt.__mul = function()
    table.insert(trace, "mul:error")
    error("arithmetic_probe")
end
local ok, message = pcall(f, box(1), box(0))
print("error", ok, message:find("arithmetic_probe", 1, true) ~= nil, table.concat(trace, ","))
