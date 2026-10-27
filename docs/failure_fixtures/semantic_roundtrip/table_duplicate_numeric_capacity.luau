-- Preserve initial allocation, store order, and fixed field arity.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local key=2.5
local t={[key]=0, ZUvxIjpsT=2, kRiMzFtsJ=3, NGCuccUFW=4, [key]=1}
print(keys(t))
t.YVUFXYsXD=5; t.DYxXyxmof=6; t[20]=20
print(keys(t))
