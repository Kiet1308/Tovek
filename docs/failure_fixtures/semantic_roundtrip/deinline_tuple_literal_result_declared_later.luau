-- Copies Luau -O2 made of a helper returning two locals and a literal
-- (`return size, offset, 0.745`). The SSA declares each copy's literal
-- result further down, right before the loop reading it (`local depthA =
-- 0.745` past both copies and `shift`): the declaration, read by nothing
-- between, is tried right after its copy, and both are `setupFlag(...)`.
local function setupFlag(part)
	local size = part.size
	local offset = part.offset
	part.size = { x = size.x, y = size.y, z = 0.01 }
	part.offset = offset
	return size, offset, 0.745
end

local a = { size = { x = 1, y = 2, z = 3 }, offset = 4 }
local b = { size = { x = 5, y = 6, z = 7 }, offset = 8 }
local sizeA, offsetA, depthA = setupFlag(a)
local sizeB, offsetB, depthB = setupFlag(b)

local function shift(offset, by)
	return { offset = offset, by = by }
end

local runs = 0
while true do
	runs += 1
	local first = shift(offsetA, -depthA)
	local second = shift(offsetB, -depthB)
	print(sizeA.x, first.offset, first.by, sizeB.y, second.offset, second.by)
	if runs == 2 then
		break
	end
end
print(a.size.z, b.size.z)
