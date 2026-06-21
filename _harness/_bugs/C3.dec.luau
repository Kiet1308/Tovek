local v = 0
local v2 = 1

for _ = 1, 6 do
	v = v2
	v2 = v + v2
end

print(v, v2)
