-- `for i = 1, n do break end` never loops back, and the next loop reuses
-- its registers: matching FORNPREP to "the one FORNLOOP on that counter"
-- found two and the decompiler failed. FORNPREP's loop is the first FORNLOOP
-- on its registers.
local function h(n, t)
    for i = 1, n do
        break
    end
    for i = 1, n do
        t[i] = i * 2
    end
    return #t
end
print(h(3, {}), h(0, {}))
local function k(n)
    local hits = 0
    for i = 1, n do
        hits += 1
        break
    end
    for i = 1, n do
        hits += i
    end
    return hits
end
print(k(3), k(0))
