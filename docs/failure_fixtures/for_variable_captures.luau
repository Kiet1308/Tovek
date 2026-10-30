-- A `for` gives every trip its own variables: a closure copying one needs no
-- snapshot local (116 `local x = i` lines left the real dumps). A write in
-- the body after the closure is seen by it, and Luau captures that variable
-- by reference.
local t = {}
for i = 1, 3 do t[i] = function() return i end end
print(t[1](), t[2](), t[3]())
local u = {}
for k, v in {10, 20} do u[k] = function() return v end end
print(u[1](), u[2]())
local w = {}
for i = 1, 2 do w[i] = function() return i end i = i * 10 end
print(w[1](), w[2]())
local nested = {}
for i = 1, 2 do
	for j = 1, 2 do
		nested[#nested + 1] = function() return i * 10 + j end
	end
end
print(nested[1](), nested[4]())
