-- `value` is computed (and the callback run) before `gate and value`;
-- folding the select into the right operand of `and` skipped the callback
-- whenever `gate` was false.
local calls = 0
local function callback()
    calls += 1
    return 42
end
local function test(choose, gate)
    local value
    if choose then
        value = false
    else
        value = callback()
    end
    return gate and value
end
print(test(false, false), calls)
print(test(false, true), calls)
print(test(true, true), calls)
