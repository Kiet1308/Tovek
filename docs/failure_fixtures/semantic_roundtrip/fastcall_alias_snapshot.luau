-- A captured owner value must keep its explicit snapshot when a local builtin alias becomes FASTCALL.

local function retainedAlias(before, after, invoke)
    local value = before
    local maximum = math.max
    local function change() value = after; return 2 end
    local saved = value
    local result = maximum(saved, (invoke(change)))
    return result, maximum(0, 0)
end

local function singleUseAlias(before, after, invoke)
    local value = before
    local maximum = math.max
    local function change() value = after; return 2 end
    local saved = value
    local result = maximum(saved, (invoke(change)))
    return result
end

local function invoke(callback) return callback() end
print("retained", pcall(retainedAlias, 10, 20, invoke))
print("single", pcall(singleUseAlias, 10, 20, invoke))
