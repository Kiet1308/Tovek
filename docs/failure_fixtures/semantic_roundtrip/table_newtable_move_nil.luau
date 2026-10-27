-- Fixed collision keys expose changes in hash allocation and nil-node reuse.
local function keys(t)
 local out = {}
 for k, v in pairs(t) do table.insert(out, tostring(k) .. "=" .. tostring(v)) end
 return table.concat(out, ",")
end
local t = { ["RIvgnQyxt"] = nil, xVDaeoSzK = tonumber("2"), QqesXlMpy = tonumber("3"), keZUMTvyB = tonumber("4"), oteYcNhCu = tonumber("5"), fAqXRWdWc = tonumber("6"), QckPuEdJi = tonumber("7"), xcOwKeRdw = tonumber("8") }
t.RIvgnQyxt = 1
print(keys(t))
