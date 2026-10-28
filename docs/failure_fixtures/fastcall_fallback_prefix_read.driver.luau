local maximum = math.max
local change
getfenv(f).math = setmetatable({}, {__index = function(_, name)
    print("lookup", name)
    change()
    return maximum
end})
local result = f(function(callback) change = callback end)
print(result)
