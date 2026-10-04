-- A `[0] = nil` entry does not keep its slot: a later insertion whose main
-- position collides may take it over, and a store through `[-0]` then
-- creates a negative key `pairs` hands out. Whether it happens depends on
-- the hash layout (`a` takes the slot, `b` and `c` do not), so a store whose
-- key is spelled differently stays a statement after a `nil` slot. A slot
-- holding a value keeps its key, and an identical spelling is safe.
local function produce()
	return "m"
end

local function moved(produce)
	local t = { [0] = nil, a = produce() }
	t[-0] = 20
	return t
end

local function moved_kept(produce)
	local t = { [0] = nil, b = produce() }
	t[-0] = 20
	return t
end

local function replaced(x)
	local t = { [0] = nil, a = 1, b = x }
	t[-0] = 20
	return t
end

local function replaced_kept(x)
	local t = { [0] = nil, c = 1, b = x }
	t[-0] = 20
	return t
end

local function same_spelling(x)
	local t = { [0] = nil, a = 1, b = x }
	t[0] = 20
	return t
end

local function held(produce)
	local t = { [-0] = 0, a = produce() }
	t[0] = 20
	return t
end

for _, build in { moved, moved_kept, replaced, replaced_kept, same_spelling, held } do
	for key, value in pairs(build(produce)) do
		if type(key) == "number" then
			print(1 / key, value)
		end
	end
end
