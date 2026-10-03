-- A parenthesised call truncates a multret to one value: `return b, (b())`
-- must yield the function and 4 only. At -O2 Luau folds `b()` to the constant 4.
local function a()
	local function b()
		return 4, 8
	end
	return b, (b())
end
print(select("#", a()))
