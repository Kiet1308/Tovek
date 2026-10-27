-- Explicit numeric keys must preserve hash placement instead of becoming an array.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local t = { [1] = nil, [2] = 2, [3] = 3 }
print(keys(t))
t.BackgroundColor3=1; t.Size=2; t.Parent=3; t[20]=20; t[3]=nil; t[21]=21
print(keys(t))
