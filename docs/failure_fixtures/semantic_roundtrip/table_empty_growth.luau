-- Fixed collision keys expose changes in hash allocation and nil-node reuse.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local t = {  }
t.ltkVlmMIy = 1
t.mjPIRHFEG = 2
t.nZywCZpcE = 3
t.SRoMftadu = 4
t.lGckgdCKl = 5
print(keys(t))
