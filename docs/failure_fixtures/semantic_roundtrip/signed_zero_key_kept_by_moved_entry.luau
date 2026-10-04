-- A store through `[-0]` updates the slot `[0]` created, which keeps its
-- key: moving the entry to the end of the constructor must not respell it
-- as `[-0]`, a negative key `pairs` would hand out.
local function produce()
	return "m"
end

local function build(produce)
	local t = {
		[0] = 0,
		marker = produce(),
	}
	t[-0] = 20
	return t
end

for key, value in pairs(build(produce)) do
	if type(key) == "number" then
		print(1 / key, value)
	end
end
