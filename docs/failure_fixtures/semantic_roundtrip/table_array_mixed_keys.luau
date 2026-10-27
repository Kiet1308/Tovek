-- Preserve initial allocation, store order, and fixed field arity.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local t={[1]=nil,alpha=10,[2]=2,beta=20,[3]=3}; print(keys(t)); t.ZUvxIjpsT=7; t.kRiMzFtsJ=8; t.NGCuccUFW=9; t[20]=20; print(keys(t))