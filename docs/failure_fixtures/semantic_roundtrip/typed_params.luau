local M = {}
function M.scale(value: number, factor: number?, label: string): string
	local result = value * (factor or 1)
	return label .. "=" .. tostring(result)
end
function M:configure(enabled: boolean, onDone: (number) -> (), items: { number }, pos: vector, buf: buffer)
	if enabled then
		onDone(#items)
	end
	return self, pos, buf
end
local function apply(callback: () -> number, count: number)
	local total = 0
	for _ = 1, count do
		total += callback()
	end
	return total
end
print(M.scale(2, 3, "x"), M.scale(5, nil, "y"))
print(apply(function() return 7 end, 3))
return M
