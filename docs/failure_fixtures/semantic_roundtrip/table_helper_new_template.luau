-- Fixed collision keys expose changes in hash allocation and nil-node reuse.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local function helper(v1, v2, v3, v4, v5, v6, v7, v8)
 return { ["ZUvxIjpsT"] = v1, kRiMzFtsJ = v2, NGCuccUFW = v3, YVUFXYsXD = v4, DYxXyxmof = v5, lxXyKTHoR = v6, WxrdAWctj = v7, ELDxjLUAA = v8 }
end
local function run(v1, v2, v3, v4, v5, v6, v7, v8)
 local t = { ZUvxIjpsT = v1, kRiMzFtsJ = v2, NGCuccUFW = v3, YVUFXYsXD = v4, DYxXyxmof = v5, lxXyKTHoR = v6, WxrdAWctj = v7, ELDxjLUAA = v8 }
 print(keys(t))
end
run(1, 2, 3, 4, 5, 6, 7, 8)
run(nil, 2, 3, 4, 5, 6, 7, 8)
run(1, nil, 3, 4, 5, 6, 7, 8)
run(1, 2, nil, 4, 5, 6, 7, 8)
run(1, 2, 3, nil, 5, 6, 7, 8)
run(1, 2, 3, 4, nil, 6, 7, 8)
run(1, 2, 3, 4, 5, nil, 7, 8)
run(1, 2, 3, 4, 5, 6, nil, 8)
run(1, 2, 3, 4, 5, 6, 7, nil)
return helper
