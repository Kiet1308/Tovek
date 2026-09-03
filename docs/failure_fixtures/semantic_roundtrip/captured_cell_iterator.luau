local cell = 0
local function it(t)
	return function(_, k)
		cell += 1
		return next(t, k)
	end
end
local function run(t)
	local reader = function() return cell end
	local acc = {}
	cell = 10
	local count = 0
	for k in it(t) do
		acc[#acc + 1] = tostring(k) .. "@" .. tostring(reader())
		count += 1
	end
	table.sort(acc)
	return table.concat(acc, ","), count, cell
end
print(run({ x = 1, y = 2 }))
print(run({}))
