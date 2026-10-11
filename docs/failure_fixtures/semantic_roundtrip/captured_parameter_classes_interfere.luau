-- SSA destruction joins every version of a captured local into one class
-- (one storage, the cell), without the interference walk ordinary copies
-- get, and gives its root and captured parameters artificial positions at
-- entry. The walk that later decides whether another class may join it
-- compares each member only with the nearest member dominating it, which is
-- sound only for interference-free classes at real definition points.
-- `swapped`: `v = #tostring(a)` joined the cell of the captured parameter
-- `b`, which is live across it (the swap reads `b` later): the output wrote
-- `p2 = #tostring(p)` unconditionally and an empty `if p2 == 1 then end`,
-- returning `5 1` for `5 2` (fr-core F3). `twoParams`: the classes of two
-- captured parameters were judged apart and merged: `local function
-- twoParams(p, p)` (fr-core F3b). A class holding a cell is now checked
-- pair by pair, and two parameters (or incoming upvalues) never share one.
local keep
local function swapped(a, b)
	if a > 0 then
		local v = #tostring(a)
		if b == 1 then
			v, b = b, v
		end
	else
		keep = function()
			return b
		end
	end
	return a, b
end
print(swapped(5, 1))
print(swapped(5, 2))
print(swapped(-1, 3), keep())

local function twoParams(a, b)
	if a < b then
		a, b = b, a
		for _ in pairs({}) do
		end
	else
		local f = function()
			return b
		end
	end
	for _ in pairs({}) do
		local _ = b % 2
	end
	local g = function()
		return a
	end
	return a, b
end
print(twoParams(1, 2))
print(twoParams(7, 3))
