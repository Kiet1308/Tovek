local function collect(t)
	local out = {}
	local found = nil
	local total = 0
	for k, v in pairs(t) do
		if v.ok then
			found = k
			break
		end
		out[#out + 1] = v
		total += v.n
	end
	return out, found, total
end
local out, found, total = collect({})
print(#out, found, total)
local out2, found2, total2 = collect({ x = { n = 1 }, y = { n = 2 } })
print(#out2, found2, total2)
local out3, found3, total3 = collect({ x = { n = 1, ok = true } })
print(#out3, found3, total3)
