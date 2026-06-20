-- parentheses truncate multret to one value; affects arg counts
local function two() return 1, 2 end
print(two())
print((two()))
local ok, a, b = pcall(function() return (two()) end)
print(ok, a, b)
print(select("#", (two())))
