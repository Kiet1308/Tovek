-- Reduced from fuzz seed 7388: a copied upvalue and a later phi share one register.
local function subject(input, flip)
    local a, cell = input, input
    local function captured()
        cell += 1
        return a
    end
    local first = captured()
    local fns = {}
    for i = 1, 3 do fns[i] = function() return i + cell end end
    for _, g in ipairs(fns) do print(g()) end
    local function identity(value)
        cell += 1
        return value
    end
    local second = identity(if flip then cell else input)
    return first, second, a, cell
end
print(subject(-3, true))
print(subject(-3, false))
