-- Copies Luau -O2 made of a helper with a default (`radius or 8`): a
-- constant argument folds it to the argument (`newCorner(frame, 6)` keeps
-- `6`) and a left-out one to the default (`newCorner(frame)` keeps `8`).
-- Each is its call again, the left-out argument left out.
function makeCorner()
	return {}
end

local function newCorner(parent, radius)
	local corner = makeCorner()
	corner.Radius = (radius or 8) * 2
	corner.Parent = parent
	return corner
end

local frame = { Name = "Frame" }
newCorner(frame, 6)
newCorner(frame)
local corner = newCorner(frame, 3)
print(corner.Radius, corner.Parent.Name)
