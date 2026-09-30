-- At -O0 a table template's keys start as `0` placeholders and fields are
-- stored through key registers. A placeholder filled after a branch must end
-- up with the stored value, once, and later fields must keep theirs.
local function new(initial, smoothTime, maxSpeed)
	local self = {
		Current = initial,
		Velocity = if type(initial) == "table" then initial else initial * 0,
		SmoothTime = smoothTime,
		MaxSpeed = if maxSpeed ~= nil then maxSpeed else math.huge,
	}
	return self
end

local a = new(3, 0.5)
local b = new({ 1 }, 0.2, 10)
print(a.Current, a.Velocity, a.SmoothTime, a.MaxSpeed)
print(b.Velocity[1], b.SmoothTime, b.MaxSpeed)
local keys = {}
for key in a do
	table.insert(keys, key)
end
table.sort(keys)
print(table.concat(keys, ","))
