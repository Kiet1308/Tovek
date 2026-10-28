-- Keep environment mutation outside the compiled subject so FASTCALL remains enabled.
local maximum = math.max
local change
getfenv(f).math = setmetatable({}, {__index = function(_, name)
    print("lookup", name)
    change()
    return maximum
end})
print(f(function(callback) change = callback end))
