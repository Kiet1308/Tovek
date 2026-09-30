-- A helper whose two returns fuse into `c and K or B` at its inlined sites,
-- next to a caller constant equal to the helper's literal: the rebuilt calls
-- must keep each site's argument and result.
local function flatten(v)
	if v.m <= 1e-6 then
		return "flat"
	end
	return v.m * 2 + v.k
end

local function steer(a, b, c)
	local EPSILON = 1e-6
	local x = flatten(a)
	local y = flatten(b)
	if c.m > EPSILON then
		local z = flatten(c)
		if c.m - EPSILON <= 1 then
			return z, y
		end
	end
	return x, y
end

print(steer({ m = 0, k = 1 }, { m = 2, k = 3 }, { m = 0.5, k = 4 }))
print(steer({ m = 1, k = 1 }, { m = 0, k = 0 }, { m = 5, k = 1 }))
