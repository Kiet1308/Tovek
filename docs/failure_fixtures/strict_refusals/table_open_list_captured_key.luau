-- O2 separates the captured-key write from the constructor's open SETLIST.
-- A refusal is preferable to changing either the key snapshot or array growth.
local function tail() return 2, 3, 4, 5, 6 end
local function stamp() print("stamp"); return nil end
local function build(key)
    local function change() key = "after"; return true end
    return { [key] = 9, [true] = stamp(), [false] = nil,
        if change() then 1 else 2, tail() }
end
local t = build("before")
t[8] = 8
print(#t, t.before, t.after)
for k, v in next, t do print(k, v) end
