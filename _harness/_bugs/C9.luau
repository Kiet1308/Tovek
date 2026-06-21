local log = {}
local function A() log[#log+1] = "A"; return 1 end
local function B(x) log[#log+1] = "B"..tostring(x); return x end
local function go(a)
	local c1 = A()
	local m = B(a)
	a = 0
	return c1 + m + a
end
print(go(7))
print(table.concat(log, ","))
