-- A comparison inside a comparison printed bare on the left (`a == b == c`):
-- exact, by left association, but the reader has to work it out. The inner
-- comparison is now parenthesized on either side.
local function check(a, b, c, x, y)
	local same = (a == b) == c
	local order = (x < y) ~= (y < 10)
	local nested = a == (b == c)
	return same, order, nested
end

print(check(1, 1, true, 1, 2))
print(check(1, 2, true, 5, 20))
print(check(3, 2, false, 12, 11))
print(check("x", "x", false, 0, 0))
