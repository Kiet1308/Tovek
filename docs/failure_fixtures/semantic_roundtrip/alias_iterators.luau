local ipairs = ipairs
local pairs = pairs
local next = next
local M = {}
function M.sum(seq)
	local total = 0
	for _, kp in ipairs(seq.Keypoints) do
		total += kp.Time
	end
	local keys = {}
	for k in pairs(seq) do
		keys[#keys + 1] = k
	end
	table.sort(keys)
	local n = 0
	for _ in next, seq.Keypoints do
		n += 1
	end
	return total, table.concat(keys, ","), n
end
print(M.sum({ Keypoints = { { Time = 1 }, { Time = 2 }, { Time = 4 } }, Name = "s" }))
