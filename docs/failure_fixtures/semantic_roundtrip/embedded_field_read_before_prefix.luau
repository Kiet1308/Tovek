-- A field of a script global (`Shared.x`) is no constant import: the
-- helper's argument call writes it before the comparison reads it.
loadstring("Shared = { x = 5 }")()
local function bump()
	Shared.x = Shared.x + 100
	return 10
end
local function cmp(p)
	if Shared.x > p then
		return "gt"
	end
	return "le"
end
local r = cmp(bump())
print(r, Shared.x)
