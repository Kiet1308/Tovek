local v = 1
local v2 = {}

while v <= 3 do
	v2[v] = function()
		return v
	end

	v += 1
end

print(v2[1](), v2[2](), v2[3]())
