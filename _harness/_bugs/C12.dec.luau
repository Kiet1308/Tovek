local v = (function()
	local result = {}

	for i = 1, 3 do
		for i2 = 1, 3 do
			for i3 = 1, 3 do
				if i3 == 2 and i2 == 2 then
					result[#result + 1] = `break-k @ {i},{i2},{i3}`
					break
				end

				if i2 == 3 and i3 == 1 then
					result[#result + 1] = `break-j @ {i},{i2},{i3}`
					break
				end

				result[#result + 1] = `{i}.{i2}.{i3}`
			end

			if i == 2 and i2 == 2 then
				result[#result + 1] = `break-outer-j @ {i},{i2}`
			end
		end
	end

	return result
end)()

for _, v2 in ipairs(v) do
	print(v2)
end

print("count", #v)
