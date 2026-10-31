-- A helper whose body makes a closure on its one line (`pcall(function()
-- ... end)`): a copy makes that closure in the caller on the helper's line,
-- which is the helper's code, not the inner function's. Line info then
-- shows the copies, so they are calls again.
local function setScale(model, scale)
	pcall(function() model.Scale = math.max(scale, 0.01) end)
end

local parts = {}
function grow(count)
	for i = 1, count do
		local part = { Scale = 0 }
		setScale(part, i * 0.5)
		parts[#parts + 1] = part
	end
	setScale(parts[1], 0.7)
	return parts[1].Scale, parts[count].Scale
end

print(grow(3))
