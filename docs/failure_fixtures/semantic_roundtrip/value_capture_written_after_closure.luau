-- A closure captures a local by value (the local is never written again
-- in source), and SSA destruction later puts another value in the same
-- output local: a swap's copy or a conditional's result. The rendered
-- closure captures that variable by reference, so it saw the later value.
-- A by-value capture of a local written after the closure is made now
-- reads a snapshot, as a loop-mutated one did (C6). Found by the typed
-- fuzzer.
local function body(input, flip)
    local a, b = input, if flip then 1 else -1
    local cell = input
    local function helper1(x)
        return (x ~= b) and a or input
    end
    local function helper3(x)
        return helper1(x)
    end
    a, b = b, a
    local function bumped()
        cell = cell + 1
        return helper1(3)
    end
    print(bumped(), cell)
    print(helper1(cell), (cell ~= b) and a or input)
    print(helper3(b), helper1(b))
end
body(2, true)
body(-0, false)

local function snapshot(input, flip, cell)
    local function current()
        return math.max(cell, if not flip then cell else input)
    end
    local function original()
        return input
    end
    cell = math.floor(-0)
    print(current(), math.max(cell, if not flip then cell else input))
    print(original(), input)
end
snapshot(-3, false, 4)
snapshot(-3, true, 4)
