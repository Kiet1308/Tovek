local function classify(value)
    if value ~= value then return "nan" end
    if value == 0 then return if 1 / value < 0 then "-zero" else "+zero" end
    return string.format("%.17g", value)
end
for _, value in {-0.0, 0.0, 0.3, 1e-310, 1e20, math.huge, -math.huge, 0 / 0} do
    print("scalar", classify(value), classify(f(value)))
end

local setter
local trace = {}
local mt = {}
local function box(value) return setmetatable({value = value}, mt) end
function mt.__mul(a, b)
    table.insert(trace, "mul:" .. a.value .. ":" .. b)
    setter(box(a.value + 1))
    return box(a.value * b)
end
function mt.__add(a, b)
    local left = if type(a) == "number" then a else a.value
    table.insert(trace, "add:" .. left .. ":" .. b.value)
    return box(left + b.value)
end
local function install(callback) setter = callback end
print("boxed", f(box(1), install).value, table.concat(trace, ","))

local multiply = mt.__mul
mt.__mul = function(a, b)
    if b == 3 then
        table.insert(trace, "mul:error:3")
        error("third_iteration")
    end
    return multiply(a, b)
end
table.clear(trace)
local ok, message = pcall(f, box(1), install)
print("error", ok, message:find("third_iteration", 1, true) ~= nil, table.concat(trace, ","))
