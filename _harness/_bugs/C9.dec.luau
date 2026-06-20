local v = {}

local function a()
	v[#v + 1] = "A"
	return 1
end

local function b(p)
	v[#v + 1] = "B" .. tostring(p)
	return p
end

print((function(p)
	local v2 = b(p)
	return a() + v2 + 0
end)(7))
print(table.concat(v, ","))
