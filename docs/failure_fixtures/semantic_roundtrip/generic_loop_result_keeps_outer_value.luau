-- A search inlined at -O2 writes its result into the caller's own variable
-- (here a parameter): on exhaustion it stores `nil`, which the variable did
-- not hold before the loop, so that store must stay.
local function find(items, wanted)
	for _, item in items do
		if item == wanted then
			return item
		end
	end
	return nil, false
end

local function run(items, wanted, flag, x, y)
	if flag then
		x, y = find(items, wanted)
	end
	return x, y
end

print(run({}, 3, true, 4, 5))
print(run({ 1, 2 }, 3, true, 4, 5))
print(run({ 1, 3 }, 3, true, 4, 5))
print(run({}, 3, false, 4, 5))
