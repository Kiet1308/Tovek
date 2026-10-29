-- V2.6 review fr-dynamic F4: at -O2 -g2 Luau inlines `bump(bump(input))` and
-- gives the inner `amount` the caller's register of `input`; the debug local
-- boundary kept a loop phi whose copy split the for step from its body, and
-- the structurer refused the whole script. Fuzz seeds 606825/606959 (generic
-- for) and 635109 (numeric for) reduce to these.
local function body(input, t)
    local cell = input
    local function bump(amount)
        cell = cell + amount
        return amount
    end
    for k in t do
        bump(bump(input))
    end
    return cell
end
local function numeric(input, n)
    local cell = input
    local function bump(amount)
        cell = cell + amount
        return amount
    end
    for i = 1, n do
        bump(bump(input))
    end
    return cell
end
print(body(1, {4, 5}), numeric(2, 3))
