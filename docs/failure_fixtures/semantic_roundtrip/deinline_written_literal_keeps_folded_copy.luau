-- A helper Luau -O2 inlined at one call, whose constants folded a builtin
-- call of it (`math.pow(10, places)` as 100, `type(value)` as "string"),
-- and called at another, too expensive to inline, with a literal there
-- (`round(x, 3)`). The written call keeps the helper's builtin calls in
-- parentheses: a recompiled copy of it must not fold what the input
-- computed at run time. The one helper body then cannot also stand for
-- the folded copy, which stays as it is.
local function round(value, places)
	local factor = math.pow(10, places)
	if type(value) ~= "string" and type(value) ~= "number" then
		print("bad", value, 1) print("bad", value, 2) print("bad", value, 3) print("bad", value, 4)
		print("bad", value, 5) print("bad", value, 6) print("bad", value, 7) print("bad", value, 8)
		error("not a number")
	end
	return math.round(value * factor) / factor
end
local x = tonumber("1.23456")
print(round("1.23456", 2), round(x, 3))
