-- A `for` whose body only breaks: Luau threads the FORGLOOP backedge through
-- that `break` to the instruction after FORGLOOP and its AUX word, so both
-- arms of the loop test reach the code after the loop. The lifter gives the
-- body arm a block of its own; it used to forward straight to the code after
-- the loop while exhaustion went through the AUX word's block, two edges
-- into the same place. A swap before the inner loop (`a, b = b, a`) that
-- only the next outer trip reads becomes copies SSA places on the edges back
-- to the outer loop: once per arm, so the body arm was no longer an empty
-- forwarder, the inner loop was no loop and the whole function failed to
-- structure ("residual goto/label would be invalid Luau", fuzz seed 164201).
-- The body arm now leaves through the AUX word's block as exhaustion does:
-- one edge, the copies written once after the inner loop.
local function generic(input, list)
	local a, b = input, 1
	for _ in list do
		a, b = b, a
		for _ in list do
			break
		end
	end
	return a, b
end

local function numeric(input, limit, list)
	local a, b = input, 1
	for _ = 1, limit do
		a, b = b, a
		for _, _ in ipairs(list) do
			break
		end
	end
	return a, b
end

local function rows(t)
	local a, b, seen = "x", "y", 0
	for _, row in pairs(t) do
		a, b = b, a
		seen += #row
		for _, _ in pairs(row) do
			break
		end
	end
	return a, b, seen
end

print(generic(3, {}), generic(3, { 1 }), generic(3, { 1, 2 }), generic(3, { 1, 2, 3 }))
print(numeric(7, 0, { 1 }), numeric(7, 1, {}), numeric(7, 3, { 1, 2 }))
print(rows({ { 1 }, {}, { 2, 3 } }))
