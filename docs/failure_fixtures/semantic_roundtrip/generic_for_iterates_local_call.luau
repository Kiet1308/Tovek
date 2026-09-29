-- A single-result call bound to a local, then iterated: the local holds only
-- the first result. Moving the call into the iterator slot (`for i in make()
-- do`) would spread `counter, 5` into the generator/state pair.
local function counter(limit, i)
	i = (i or 0) + 1
	if i <= (limit or 3) then
		return i
	end
	return nil
end

local function make()
	return counter, 5
end

local seen = {}
local it = make()
for i in it do
	table.insert(seen, i)
end
print(#seen)

local total = 0
for i in make(), nil, nil do
	total += i
end
print(total)
