-- `x + change()` reads the register `x` when `+` runs, after `change` wrote
-- it; the identical body of `helper` fetches the upvalue `x` first. Calling
-- `helper()` in place of the copy printed `1` instead of `2`.
local x = 1
local function change()
    x = 2
    return 0
end

local function helper()
    print("value", x + change())
end

print("value", x + change())
return helper
