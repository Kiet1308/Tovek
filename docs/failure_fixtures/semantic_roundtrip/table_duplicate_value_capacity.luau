-- Fixed collision keys expose changes in hash allocation and nil-node reuse.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local key = "ZUvxIjpsT"
local t = { [key] = 0, kRiMzFtsJ = 2, NGCuccUFW = 3, YVUFXYsXD = 4, DYxXyxmof = 5, lxXyKTHoR = 6, WxrdAWctj = 7, ELDxjLUAA = 8, [key] = 1, "array" }
print(keys(t))
