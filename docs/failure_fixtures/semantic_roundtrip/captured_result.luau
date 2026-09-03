local function capture(t)
	local fns = {}
	for k, v in pairs(t) do
		if k == "Value" then
			v *= 2
		end
		fns[#fns + 1] = function() return k, v end
		v += 1
	end
	return fns
end
local fns = capture({ Value = 10, Other = 3 })
local seen = {}
for _, f in ipairs(fns) do
	local k, v = f()
	seen[k] = v
end
print(seen.Value, seen.Other)
local function gsubCapture(str, data)
	for k, v in pairs(data) do
		if k == "Value" then
			v = v * 3
		end
		str = string.gsub(str, "$" .. k, function() return tostring(v) end)
	end
	return str
end
print(gsubCapture("a=$Value b=$Other", { Value = 2, Other = 5 }))
