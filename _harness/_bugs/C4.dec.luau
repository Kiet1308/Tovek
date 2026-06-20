local v = 0

local function step(p)
	v += p
	return v
end

local v2 = v
local v3 = {}

for i = 1, 5 do
	v3[#v3 + 1] = step(i)
end

print(table.concat(v3, ","), v2)
