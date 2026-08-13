-- Generalized iteration, float-step numeric for, ipairs with continue+break,
-- custom closure iterator triple, sorted pairs output, type assertions.
local log = {}
local t = { foo = 1, bar = 2, baz = 3 }
for k, v in t do
	log[#log + 1] = k .. "=" .. v
end
table.sort(log)
print(table.concat(log, ","))
local n = 4
local acc = 0
for i = 1, n, 0.5 do
	acc += i
end
print(acc)
local items = { 10, 20, 30, 40, 50 }
local out = {}
for i, v in ipairs(items) do
	if v == 30 then continue end
	if v > 40 then break end
	out[#out + 1] = v
end
print(table.concat(out, ","))
local p = {}
p["a"] = 1
p["b"] = 2
local ks = {}
for k in pairs(p) do
	ks[#ks + 1] = k
end
table.sort(ks)
print(table.concat(ks, ","))
local function gen(seed)
	return function(state, prev)
		prev += 1
		if prev > state then return nil end
		return prev
	end, seed, 0
end
local acc2 = 0
for v in gen(3) do
	acc2 += v
end
print(acc2)
local z = (5 :: number) + 1
local q = ("12" :: string) .. "x"
print(z, q)
const w = 7
print(w * 2)