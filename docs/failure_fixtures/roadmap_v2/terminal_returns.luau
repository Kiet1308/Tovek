local function scalar(flag, api)
    local result
    if flag then result = api.next("yes") else result = api.next("no") end
    return result
end

local function conditional(flag, api)
    local result
    if flag then
        api.event("branch")
        if api.test() then result = api.next("inner") else result = false end
    else
        result = nil
    end
    return result
end

local function andValue(input, api)
    if input then local result = api.next("and"); return result end
    return input
end

local function orValue(input, api)
    if not input then local result = api.next("or"); return result end
    return input
end

local function andGuard(input, api)
    if not input then return input end
    local result = api.next("guard-and")
    return result
end

local function orGuard(input, api)
    if input then return input end
    local result = api.next("guard-or")
    return result
end

local function tuple(input, api, ...)
    if input then return api.next("tuple") end
    return ...
end

local function scalarVararg(input, ...)
    local result
    if input then result = ... else result = nil end
    return result
end

local function captured(input, api)
    local result
    api.keep(function() return result end)
    if input then result = api.next("captured") else result = false end
    return result
end

local function parameter(input, api)
    if input then input = api.next("parameter") end
    return input
end

local function fields(data, api)
    local result = api.check(data.X)
    if not result then return result end
    result = api.check(data.Y)
    if result then result = api.check(data.Z) end
    return result
end

local function ordered(left, right)
    local result
    if left < right then result = false else result = true end
    return result
end

return {
    scalar = scalar, conditional = conditional, andValue = andValue, orValue = orValue,
    andGuard = andGuard, orGuard = orGuard, tuple = tuple, scalarVararg = scalarVararg,
    captured = captured, parameter = parameter, fields = fields, ordered = ordered
}
