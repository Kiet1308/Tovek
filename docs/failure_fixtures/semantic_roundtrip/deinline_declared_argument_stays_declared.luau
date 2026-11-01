-- A local read once as the argument of a copy. Luau passes the local's
-- register to the copy as it is, so `local humanoid = c and c.Humanoid;
-- isAlive(humanoid)` and `isAlive(c and c.Humanoid)` give the same bytecode,
-- the copy's own jumps on the helper's lines: a guarded value keeps its
-- local, as sources almost always write it; a plain field read goes into the
-- call. `near`, defined right below the helper it holds copies of, keeps its
-- blank line above: the copies' lines are the helper's, not the function's.
local function isAlive(humanoid)
	return humanoid ~= nil and humanoid.Health > 0
end

local function near(characters, origin)
	local found = {}
	for _, character in characters do
		local humanoid = character and character.Humanoid
		if not isAlive(humanoid) then
			continue
		end
		local distance = math.abs(character.X - origin)
		if distance <= 12 then
			table.insert(found, character.Name)
		end
	end
	return found
end

local function first(characters)
	for _, character in characters do
		local humanoid = character.Humanoid
		if isAlive(humanoid) and character.X > 0 then
			return character.Name
		end
	end
	return nil
end

local characters = {
	{ Name = "a", X = 3, Humanoid = { Health = 100 } },
	{ Name = "b", X = 40, Humanoid = { Health = 100 } },
	{ Name = "c", X = 5, Humanoid = { Health = 0 } },
	{ Name = "d", X = 9 },
}
print(table.concat(near(characters, 0), ","), first(characters))
