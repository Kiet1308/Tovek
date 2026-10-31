-- A select whose arms are both truthy reads with its condition as written:
-- `spread and random:NextNumber(-spread, spread) or 0` copied into a larger
-- expression, which SSA lays out as `if not spread then 0 else (... or
-- 0)`, is that select again (`c and (b or k) or k` is `c and b or k`),
-- and so the helper's call, rather than `not spread and 0 or ... or 0`.
local random = { seed = 3 }
function random:NextNumber(a, b)
	self.seed = (self.seed * 48271) % 2147483647
	return a + (b - a) * (self.seed / 2147483647)
end

local function getRandom(spread)
	return spread and random:NextNumber(-spread, spread) or 0
end

function place(origin, radius, spread)
	local x = origin + (radius + getRandom(spread)) * 2
	local y = origin - (radius + getRandom(spread))
	return x, y, (radius + getRandom(spread)) / 2
end

print(place(1, 2, 0.5))
print(place(1, 2, nil))
