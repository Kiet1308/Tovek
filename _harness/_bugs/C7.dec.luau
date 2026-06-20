local function a()
	return 1, 2, 3
end

local function b()
	return 9
end

local function choose(p, ...)
	return not p and 9 or a()
end

print("true:", choose(true))
print("false:", choose(false))
print("tcount:", select("#", choose(true)))
