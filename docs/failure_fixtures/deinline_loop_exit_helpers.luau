-- Value helpers that return from inside a loop after a guard `return`
-- (fr-core F6 and its variants). Luau -O2 inlines each call: a `return`
-- inside the loop becomes a store and a `break` (or a flag), the guard an
-- if/else around the loop. The copies are rebuilt as calls.
local t = { false, true, false, true }
local g = 0
local function repeated(x)
	if t[x] then
		return -1
	end
	repeat
		g += 1
		if t[x + 1] then
			return g
		end
	until g >= 5
	return -2
end
local function tested(x)
	if t[x] then
		return -1
	end
	while g < 9 do
		g += 1
		if t[x + 1] then
			return g
		end
	end
	return -2
end
local function find(list, wanted)
	if not list then
		return nil
	end
	for i, value in list do
		if value == wanted then
			return i
		end
	end
	return 0
end
print(repeated(1), repeated(2), repeated(3))
print(tested(1), tested(2), tested(3))
print(find(t, true), find(nil, true), find({}, 1))
