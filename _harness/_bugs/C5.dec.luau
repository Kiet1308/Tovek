local function two()
	return 1, 2
end

print(two())
print((two()))
local success, result, v = pcall(function()
	return two()
end)
print(success, result, v)
print(select("#", (two())))
