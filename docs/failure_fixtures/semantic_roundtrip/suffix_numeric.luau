local function nearest(list)
	local best = math.huge
	local bestItem = nil
	local seen = {}
	local count = 0
	local label = "x"
	for i = 1, #list do
		local d = list[i].d
		if d < best then
			best = d
			bestItem = list[i]
		end
		seen[i] = true
		count += 1
		label ..= tostring(i)
	end
	return bestItem, best, seen, count, label
end
local item, best, seen, count, label = nearest({ { d = 5 }, { d = 2 }, { d = 9 } })
print(item.d, best, #seen, count, label)
local item2, best2, seen2, count2, label2 = nearest({})
print(item2, best2, #seen2, count2, label2)
