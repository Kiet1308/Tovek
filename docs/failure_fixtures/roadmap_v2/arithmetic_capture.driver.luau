local setter
local trace = {}
local mt = {}
local function box(value) return setmetatable({value = value}, mt) end
function mt.__lt(a, b)
    table.insert(trace, "lt:" .. a.value)
    setter(box(10))
    return false
end
function mt.__mul(a, b)
    table.insert(trace, "mul:" .. a.value)
    return a.value * b
end
local result = f(box(1), box(0), function(callback) setter = callback end)
print(result, table.concat(trace, ","))
