-- An owning captured register can be read after the value callback changes it.
local function tail() return 2, 3 end
local function stamp() print("stamp"); return nil end
local function build(key, flag)
    local function change() key = "after"; print("change"); return 9 end
    return { [key] = change(), later = stamp(), if flag then stamp() else change(), tail() }
end
local t = build("before", true)
print(t.before, t.after, #t)
