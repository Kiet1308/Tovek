local fns = {}
local i = 1
while i <= 3 do
	local x = i
	fns[i] = function() return x end
	i += 1
end
print(fns[1](), fns[2](), fns[3]())
