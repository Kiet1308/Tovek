local function choose(model, flag)
    local chosen
    if flag then chosen = model.Head else chosen = model.Tail end
    print(chosen)
    return chosen
end

local function same(left, right, flag)
    local chosen
    if flag then chosen = left.Head else chosen = right.Head end
    print(chosen)
    return chosen
end

local function unknown(model, other, flag)
    local chosen
    if flag then chosen = model.Head else chosen = other end
    print(chosen)
    return chosen
end

local model = { Head = 11, Tail = 22 }
for _, flag in { false, true } do
    print(choose(model, flag), same(model, { Head = 33 }, flag), unknown(model, 44, flag))
end

local function magnitude(model)
    local weight = model.Weight
    if weight < 0 then weight = -weight end
    weight += 2
    print(weight)
    return weight
end
print(magnitude({ Weight = -4 }))

local function normalized(model)
    local dot = model:Dot(1)
    if dot < 0 then dot = -dot end
    print(dot)
    return dot
end
print(normalized({ Dot = function(_, x) return x - 6 end }))

local none = {}
local function getFor(self, key)
    local value = self._perPlayer[key]
    if value == nil then value = self._value
    elseif value == none then value = nil end
    return value
end
local owner = setmetatable({ _perPlayer = { empty = none, actual = 17 } }, {
    __index = function(_, key)
        print("get", key)
        return 33
    end,
})
print(getFor(owner, "empty"), getFor(owner, "actual"), getFor(owner, "missing"))
