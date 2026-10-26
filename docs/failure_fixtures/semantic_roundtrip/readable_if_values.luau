local function choose(flag, first, second)
    if flag then return first else return second end
end
local function many()
    return 1, nil, 3
end
local function arity(flag)
    if flag then return many() else return nil end
end
local function booleanize(value)
    if value then return true end
    return false
end
for _, flag in {false, true, 0, "", {}} do
    print("boolean", booleanize(flag), type(booleanize(flag)))
    print("select", choose(flag, false, nil), choose(flag, "a", "b"))
    print("arity", select("#", arity(flag)), arity(flag))
    local mode
    if flag then mode = "on" else mode = "off" end
    print("mode", mode)
end
local mode = "outer"
do
    local mode
    if mode then mode = "unexpected" else mode = "inner" end
    print("scope", mode)
end
print("scope", mode)
local count = 0
local function condition()
    count += 1
    return count % 2 == 0
end
local function ordered()
    local value
    if condition() then value = false else value = nil end
    return value
end
print("effects", ordered(), ordered(), count)
