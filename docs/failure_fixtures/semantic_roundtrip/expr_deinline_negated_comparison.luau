-- Copies Luau -O2 made of `isWordChar(c)` (`return c:match("[%w_]") ~=
-- nil`) under `not`: Luau folds `not (x ~= nil)` into `x == nil`, so the
-- copy is the comparison turned over. It is `not isWordChar(c)` again, the
-- plain copy `isWordChar(c)`.
local function isWordChar(c)
	return c:match("[%w_]") ~= nil
end

local function countBreaks(text)
	local breaks = 0
	for i = 1, #text do
		local c = text:sub(i, i)
		if not isWordChar(c) and c ~= "-" then
			breaks += 1
		end
	end
	return breaks, isWordChar(text:sub(1, 1))
end

print(countBreaks("hello world-x"))
print(countBreaks("_a b"))
