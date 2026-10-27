local function booleanValue(value)
    return not value
end
local function choice(flag)
    if flag then return 1 end
    return "one"
end
local function tuple()
    return 3, "three", nil
end
local function maybe(flag)
    if flag then return 5 end
end
local function differentArity(flag)
    if flag then return nil end
    return
end
local function unknownOperator(value: number)
    return value + 1
end
local function unknownLibrary()
    return math.floor(1.25)
end
local oldMath = math
math = {floor = function() return "hooked" end}
local value = setmetatable({}, {__add = function() return "metamethod" end})
for _, flag in {true, false} do
    print(booleanValue(flag), choice(flag), maybe(flag))
    print("arity", select("#", differentArity(flag)))
end
print("tuple", select("#", tuple()), tuple())
print("unknown", unknownOperator(value), unknownLibrary())
math = oldMath
local methods = {}
print("method-before", methods.test)
function methods:test(value)
    return value == self
end
print("method", methods:test(methods))
local function nested()
    local inner = function(value) return not value end
    return inner(false)
end
print("nested", nested())
return booleanValue, choice, tuple, maybe, differentArity, unknownOperator, unknownLibrary
