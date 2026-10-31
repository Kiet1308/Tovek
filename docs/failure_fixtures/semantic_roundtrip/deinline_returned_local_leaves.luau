-- A helper returning its own local, stored last on every path of an `if`
-- (`local ok; if p == p and p ~= math.huge then ok = p ~= -math.huge else
-- ok = false end; return ok`, as the compiler leaves `return p == p and
-- ...`): its copies store those leaves into their result, declared there
-- or a local the caller has, as an `and` chain of copies does
-- (`IsFinite(v.x) and IsFinite(v.y) and IsFinite(v.z)`).
local function IsFinite(p)
	return p == p and p ~= math.huge and p ~= -math.huge
end

local function IsFiniteVector3(v)
	return IsFinite(v.x) and IsFinite(v.y) and IsFinite(v.z)
end

function check(points)
	local count = 0
	for _, point in points do
		if IsFiniteVector3(point) then
			count += 1
		end
	end
	return count, IsFiniteVector3(points[1])
end

print(check({ { x = 1, y = 2, z = 3 }, { x = 0 / 0, y = 1, z = 1 }, { x = 1, y = math.huge, z = 1 }, { x = 1, y = 1, z = -math.huge } }))
