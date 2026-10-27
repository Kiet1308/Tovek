-- Fixed collision keys expose changes in hash allocation and nil-node reuse.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local function make(v1, v2, v3, v4, v5)
local t = { ZUvxIjpsT = v1, kRiMzFtsJ = v2, NGCuccUFW = v3, YVUFXYsXD = v4, DYxXyxmof = v5 }
t.lxXyKTHoR = 6
t.WxrdAWctj = 7
t.ELDxjLUAA = 8
return t
end
print(keys(make(1, 2, 3, 4, 5)))
print(keys(make(nil, 2, 3, 4, 5)))
print(keys(make(1, nil, 3, 4, 5)))
print(keys(make(1, 2, nil, 4, 5)))
print(keys(make(1, 2, 3, nil, 5)))
print(keys(make(1, 2, 3, 4, nil)))