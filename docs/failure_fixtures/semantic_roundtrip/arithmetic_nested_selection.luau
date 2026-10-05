-- Nested if/and/or selections must retain their guards, numeric operation
-- order and skipped arms when an equivalent bytecode helper is reconstructed.
local function nestedCurve(value, factor)
    return -((if value > 0 then factor * 2 + (if value < 5 then 3 else 4) else 1) - 7)
end

local trace = {}
local factor = setmetatable({}, {
    __mul = function(_, amount)
        trace[#trace + 1] = "multiply:" .. tostring(amount)
        return amount * 4
    end,
})

-- Zero is truthy in Luau. NaN must not turn `not (x > 0)` into `x <= 0`.
-- The factor's metamethod runs once on a positive arm and never otherwise.
for _, value in { -3, -0.0, 0.0, 2, 5, 0 / 0, -math.huge, math.huge } do
    trace = {}
    print("helper", nestedCurve(value, factor), table.concat(trace, ","))
    trace = {}
    print("copy", -(((not (value > 0) and 1 or factor * 2 + (if not (value < 5) then 4 else 3))) - 7), table.concat(trace, ","))
end

-- A throwing metamethod in the unselected arm must stay unevaluated.
local throwing = setmetatable({}, {
    __mul = function()
        error("selected arithmetic arm", 0)
    end,
})
for _, value in { -1, 1 } do
    print("helper throws", pcall(function()
        return nestedCurve(value, throwing)
    end))
    print("copy throws", pcall(function()
        return -(((not (value > 0) and 1 or throwing * 2 + (if not (value < 5) then 4 else 3))) - 7)
    end))
end

return nestedCurve
