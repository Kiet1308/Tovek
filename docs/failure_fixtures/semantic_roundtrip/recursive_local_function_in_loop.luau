-- A recursive local function created inside a loop or a branch: the
-- closure captures its own register, which NEWCLOSURE fills before it reads
-- the captures, so the value captured is the new closure. A snapshot taken
-- before the closure exists (`local copy = f; f = function() copy() end`)
-- holds nil, and the recursion calls it. Found by the typed fuzzer; the
-- shape is the deep copy helper of a game's table utilities.
local function sync(target, template)
	local clone = table.clone(target)
	for key, item in pairs(template) do
		if type(item) == "table" then
			local function deepCopy(value)
				local copy = table.clone(value)
				for innerKey, inner in copy do
					if type(inner) == "table" then
						copy[innerKey] = deepCopy(inner)
					end
				end
				return copy
			end
			clone[key] = deepCopy(item)
		else
			clone[key] = item
		end
	end
	return clone
end

local result = sync({ a = 1 }, { b = { c = { d = 2 } }, e = 3 })
print(result.a, result.b.c.d, result.e)

local function run(count)
	for index = 1, count do
		local function self(item)
			return self, item
		end
		local got, item = self(index)
		print(got == self, item)
	end
end
run(2)

-- Assigned again after the closure: the closure keeps the value it captured.
local function rebound()
	local found = {}
	for index = 1, 2 do
		local helper
		helper = function()
			return helper
		end
		found[index] = helper
		helper = index
		print(found[index]() == found[index], helper)
	end
end
rebound()
