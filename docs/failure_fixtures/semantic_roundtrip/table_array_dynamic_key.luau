-- Preserve initial allocation, store order, and fixed field arity.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local function make(k) return { 1, [k]=nil, 3 } end
local t1=make(1); print(keys(t1)); t1.ZUvxIjpsT=7; t1.kRiMzFtsJ=8; t1.NGCuccUFW=9; t1[20]=20; print(keys(t1))
local t2=make(2); print(keys(t2)); t2.ZUvxIjpsT=7; t2.kRiMzFtsJ=8; t2.NGCuccUFW=9; t2[20]=20; print(keys(t2))
local t3=make(3); print(keys(t3)); t3.ZUvxIjpsT=7; t3.kRiMzFtsJ=8; t3.NGCuccUFW=9; t3[20]=20; print(keys(t3))

local function branch(value)
 print("branch", value)
 return value
end
local function emptyTail() return end
local function build(k, tail)
 return { [k] = 9, if k then branch(1) else branch(2), tail() }
end
for _, k in ipairs({1, 2, 3, "extra"}) do
 print(keys(build(k, emptyTail)))
end
