local state = 0
local function step(delta)
	state = state + delta
	return state
end
local trail = {}
for i = 1, 5 do
	trail[#trail + 1] = step(i)
end
print(table.concat(trail, ","), state)