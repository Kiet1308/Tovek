-- V2.7 late presentation: guards, bottom-tested loops, and one-shot loops.
-- The observations also protect the cases where a pretty rewrite is unsafe.
local trace = {}
local function emit(value)
    trace[#trace + 1] = tostring(value)
end

local function guarded(value, limit)
    if value < limit then
        return "small"
    else
        emit(value)
    end
    return "kept"
end
for _, value in {1, 4, 0 / 0} do
    print("guard", guarded(value, 3))
end

local function bottom(items, limit)
    local index = 0
    repeat
        index += 1
        if index > #items or limit < items[index] then break end
        emit(items[index])
    until #items <= index
end
bottom({}, 4)
bottom({1, 2, 9}, 4)
bottom({1, 2, 3}, 4)

-- A continue of the while loop skips its last condition entirely. Changing
-- this to repeat would call done() once too often and could exit too soon.
local step, probes = 0, 0
local function done()
    probes += 1
    emit("test:" .. step)
    return step >= 4
end
while true do
    step += 1
    if step % 2 == 1 then continue end
    emit("body:" .. step)
    if done() then break end
end
print("continue", step, probes)

-- Inner-loop continues target only that loop; they do not skip the outer
-- bottom test. The outer loop can still recover as a repeat.
local outer = 0
while true do
    outer += 1
    for inner = 1, 3 do
        if inner == 2 then continue end
        emit(outer .. ":" .. inner)
    end
    if outer >= 2 then break end
end

-- Flattening a one-shot body cannot retarget another break to its parent.
for _, stop in {true, false} do
    while true do
        if stop then break end
        emit("one-shot")
        break
    end
    emit("after:" .. tostring(stop))
end

-- Keep the fresh scope of captured locals and a shadowed global read.
local readers = {}
for index = 1, 3 do
    while true do
        local saved = index
        readers[#readers + 1] = function() return saved end
        break
    end
    while true do
        local tostring = function() return "shadow:" .. index end
        emit(tostring())
        break
    end
    emit(tostring(index))
end
for _, read in readers do emit(read()) end
print(table.concat(trace, ","))
