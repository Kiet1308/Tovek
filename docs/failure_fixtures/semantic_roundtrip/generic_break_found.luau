local function pick(pets, all)
	for k in pairs(pets) do
		local found = nil
		for _, pet in ipairs(all) do
			if pet.GUID == k then
				found = pet
				break
			end
		end
		if not found then
			pets[k] = nil
		else
			print("keep", k, found.Name)
		end
	end
	return pets
end
local pets = { a = true, b = true, c = true }
local all = { { GUID = "a", Name = "A" }, { GUID = "c", Name = "C" } }
pick(pets, all)
local keys = {}
for k in pairs(pets) do keys[#keys + 1] = k end
table.sort(keys)
print(table.concat(keys, ","))
