local print = print
local t = {}
local i = 0
while i < 2 do
	i += 1
	local f = print
	t[i] = function(x)
		f(x)
	end
end
print(t[1] == t[2])
t[1]("a")
t[2]("b")
