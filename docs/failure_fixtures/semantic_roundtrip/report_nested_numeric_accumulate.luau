-- Reported together with the nested-loop failures (user bug report).
local function f()
	local t = {}
	for i = 0, 255 do
		local x = i
		for j = 1, 8 do
			x = x + j
		end
		t[i + 1] = x
	end
	return t
end
local t = f()
print(#t, t[1], t[2], t[256])
