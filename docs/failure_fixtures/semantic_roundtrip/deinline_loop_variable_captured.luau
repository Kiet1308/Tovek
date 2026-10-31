-- A helper made in each iteration of a loop, reading the loop's variable
-- by value: only the loop rebinds it, so the copies after the helper in the
-- same iteration read what it captured, and are its calls.
function parse(rows)
	local out = {}
	for _, row in rows do
		local names = { "a", "b", "c" }
		local function getValue(name)
			local index = table.find(names, name)
			return index and row[index] or 0
		end
		out[#out + 1] = getValue("a") + getValue("b") * getValue("c")
	end
	return out
end

local result = parse({ { 1, 2, 3 }, { 4, 5, 6 } })
print(result[1], result[2])
