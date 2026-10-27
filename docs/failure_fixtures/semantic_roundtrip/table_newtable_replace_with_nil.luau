-- Fixed collision keys expose changes in hash allocation and nil-node reuse.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local t = { ["jqXAufKiY"] = 0, NlXgWaexY = 2, TduqiWrmU = 3, qPfyOQwCG = 4, ydWDmdoTr = 5, QZNQgFuUN = 6, YKiZYJtrY = 7, HUDYRqWsv = 8 }
t.jqXAufKiY = nil
print(keys(t))
