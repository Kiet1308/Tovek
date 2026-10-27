-- Fixed collision keys expose changes in hash allocation and nil-node reuse.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local t = { ["zfdflLZkY"] = 1, ["MHalLDEKy"] = 2, ["pYIFnTGjP"] = 3 }
t.sedcMuScq = 4
t.EIvzfWsGJ = 5
t.vgAMPhVUt = 6
t.rDjvKiAoJ = 7
t.MVIcLKpPL = 8
print(keys(t))
