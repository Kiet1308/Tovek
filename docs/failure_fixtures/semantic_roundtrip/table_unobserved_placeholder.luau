local function value(n)
 print("value", n)
 return n
end
local function keys(t)
 local out = {}
 for k, v in pairs(t) do out[#out + 1] = k .. "=" .. tostring(v) end
 return table.concat(out, ",")
end
local function private(flag)
 local t = { Selected = 0, Other = 0 }
 if flag then t.Selected = value(1) else t.Selected = value(2) end
 return t
end
local function observed(flag)
 local t = { Selected = 0, Other = 0 }
 print("before", t.Selected)
 if flag then t.Selected = value(1) else t.Selected = value(2) end
 return t
end
local function captured(flag)
 local t = { Selected = 0, Other = 0 }
 local function read() return t.Selected end
 print("captured", read())
 if flag then t.Selected = value(1) else t.Selected = value(2) end
 return t, read
end
local function grows(flag)
 local t = { Selected = 0 }
 t.Added = value(3)
 if flag then t.Selected = value(1) else t.Selected = value(2) end
 return t
end
local function maybe(flag)
 local t = { Selected = 0, Other = 0 }
 if flag then t.Selected = value(1) end
 return t
end
for _, flag in { false, true } do
 print("private", keys(private(flag)))
 print("observed", keys(observed(flag)))
 local t, read = captured(flag)
 print("after", keys(t), read())
 print("grows", keys(grows(flag)))
 print("maybe", keys(maybe(flag)))
end
