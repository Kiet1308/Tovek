local function capBreak(list)
	local fns = {}
	for i, v in ipairs(list) do
		if v == 0 then
			continue
		end
		v = v * 10
		fns[#fns + 1] = function() return v end
		if v > 200 then
			break
		end
	end
	local out = {}
	for _, f in ipairs(fns) do out[#out + 1] = f() end
	return table.concat(out, ",")
end
print(capBreak({ 1, 0, 2, 30, 4 }))
print(capBreak({}))
local function retCapture(list)
	for _, v in ipairs(list) do
		if v > 1 then
			return function() return v end
		end
	end
	return nil
end
local f = retCapture({ 1, 5, 9 })
print(f and f())
