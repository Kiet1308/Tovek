-- A split constructor needs the VM's bulk array growth, including its tail.
local events = {}
local function branch(value)
    events[#events + 1] = "branch"
    return value
end
local function tail()
    events[#events + 1] = "tail"
    return 2, 3, 4, 5, 6
end
local function build(key)
    return { [key] = 9, [true] = nil, [false] = nil,
        if key then branch(1) else branch(2), tail() }
end
local savedTable = table
table = { concat = table.concat, pack = function() error("unexpected fallback pack") end }
for _, key in {1, 2, 3, "extra", false} do
    events = {}
    local t = build(key)
    t[8] = 8
    print(#t, table.concat(events, ","))
    for k, v in next, t do print(type(k), k, v) end
end
events = {}
print(pcall(build), #events)
print(pcall(build, 0 / 0), #events)
table = savedTable

-- Values already stored must stay snapshots, with shared table identity.
local held = 17
local function change() held = 23; return 1 end
local function emptyTail() return end
local function snapshot(callback)
    local t = { [2] = held, [0] = nil, change(), callback() }
    return t
end
local t = snapshot(emptyTail)
print(t[1], t[2], held)
local value = {}
local function initial() print("initial"); return value end
local function identity(key)
    return { [2] = initial(), [0] = nil,
        if key then branch(1) else branch(2), emptyTail() }
end
print(identity(false)[2] == value)

-- SSA versions of a captured value still refer to one mutable cell.
local function captured(value)
    local function update() value = 99; return true end
    local t = { Value = value, [true] = nil, [false] = nil,
        if update() then 1 else 2, tail() }
    return t.Value, value
end
print(captured(17))

local function capturedKey(key)
    local function update() key = "after"; return true end
    return { [key] = 9, [true] = nil, [false] = nil,
        if update() then 1 else 2, tail() }
end
local keyedTable = capturedKey("before")
keyedTable[8] = 8
print(#keyedTable, keyedTable.before, keyedTable.after)

-- A dynamic-key error precedes a straight-line call as well as a branch.
local calls = 0
local function mark() calls += 1; return 3 end
local function keyed(key) return { [key] = 9, mark(), tail() } end
print(pcall(keyed), calls)
print(pcall(keyed, 0 / 0), calls)
