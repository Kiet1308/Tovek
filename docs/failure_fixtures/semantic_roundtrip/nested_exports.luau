local function search(groups, target)
	local hits = {}
	local last = nil
	for gi, group in ipairs(groups) do
		local found = nil
		local idx = 0
		for i, v in ipairs(group) do
			if v == target then
				found = v
				idx = i
				break
			end
		end
		if found then
			hits[#hits + 1] = gi * 100 + idx
			last = found
		end
	end
	return hits, last
end
local hits, last = search({ { 1, 2 }, { 3, 4, 2 }, { 5 } }, 2)
print(table.concat(hits, ","), last)
local hits2, last2 = search({}, 2)
print(#hits2, last2)
