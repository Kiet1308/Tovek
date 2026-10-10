-- Helpers below the floors whose copies line info shows (plan E1), matched
-- by the expression de-inliner: `sorter("Order", true)` returns a table
-- holding a function literal (one anchor), and each copy stores it into a
-- field: rebuilt at its three stores. `ratio(a, b)` sits in both arms of an
-- `or` whose value the caller adds to; Luau leaves that addition on the
-- helper's line, which is no third copy: both copies rebuilt.
-- Negatives: `byHand` builds the same table by hand in a function holding
-- no copy of `sorter`: it stays. `twice` holds one copy of `ratio` and a
-- hand-written `b / (a + b)`: two matches for one copy, both stay.
local function compare(x, y, ascending)
	if ascending then
		return x < y
	end
	return x > y
end

local function sorter(field, ascending)
	return {
		Compare = function(a, b)
			return compare(a[field] or 0, b[field] or 0, ascending)
		end,
	}
end

local function ratio(a, b)
	return a / (b + a)
end

local M = {}
function M.build()
	local sorts = {}
	sorts.Type = sorter("TypeOrder", true)
	sorts.Sub = sorter("SubOrder", true)
	sorts.Old = sorter("Time", false)
	return sorts
end
function M.byHand(field)
	return {
		Compare = function(a, b)
			return compare(a[field] or 0, b[field] or 0, true)
		end,
	}
end
function M.mix(a, b, c)
	return (c and ratio(a, b) or ratio(b, a)) + 1
end
function M.twice(a, b)
	return ratio(a, b), b / (a + b)
end

local sorts = M.build()
local hand = M.byHand("TypeOrder")
print(sorts.Type.Compare({ TypeOrder = 1 }, { TypeOrder = 2 }), sorts.Old.Compare({ Time = 1 }, { Time = 2 }))
print(sorts.Type == sorts.Sub, sorts.Type.Compare == sorts.Sub.Compare, hand.Compare({}, { TypeOrder = 3 }))
print(M.mix(1, 3, true), M.mix(1, 3, false), M.twice(2, 6))
