local function eagerOr(before, after, invoke)
    local value = before
    local function change() value = after; return 2 end
    local function inner() return (value or value) + invoke(change) end
    return inner()
end

local function eagerAnd(before, after, invoke)
    local value = before
    local function change() value = after; return 2 end
    local function inner() return (value and value) + invoke(change) end
    return inner()
end

local function lateOwner(before, after, invoke)
    local value = before
    local function change() value = after; return 2 end
    return value + invoke(change)
end

local function invoke(callback) return callback() end
print("or", pcall(eagerOr, 10, 20, invoke))
print("and", pcall(eagerAnd, 10, 20, invoke))
print("late", pcall(lateOwner, 10, 20, invoke))
