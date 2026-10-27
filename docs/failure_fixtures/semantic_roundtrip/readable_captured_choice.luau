-- Late readable selects must not remove the writes that keep this capture fresh.
-- Vararg factories retain one literal called repeatedly, including at -O2.
local flag = tonumber("1") == 1
local options = { false, "no" }
local choice
if flag then
    choice = options[1]
else
    choice = options[2]
end

local function fresh(...)
    return function()
        return choice
    end
end

local first, second = fresh(), fresh()
print("fresh", first == second, first(), second())

-- An unwritten module local is a control: its closure must retain sharing.
local fixed = tostring(17)
local function shared(...)
    return function()
        return fixed
    end
end

local left, right = shared(), shared()
print("shared", left == right, left(), right())
