-- Copies Luau -O2 made of `compareNumbers(a, b, true)` as the whole body
-- of a sorter, its value returned by the caller: the constant folded the
-- `descending and (...) or (...)` select to its first arm (`X or Y` with
-- `X` truthy is `X`), and the returned copy is matched by the helper's
-- specializations, its returns as stores into one result. Each is
-- `return compareNumbers(..., true)` again.
local function compareNumbers(a, b, descending)
	if a == b then
		return nil
	end
	if descending then
		return if b < a then -1 else 1
	else
		return if a < b then -1 else 1
	end
end

local sorters = {
	Rank = {
		Compare = function(x, y)
			return compareNumbers(x.Rank, y.Rank, true)
		end,
	},
	Quality = {
		Compare = function(x, y)
			return compareNumbers(x.Quality or 0, y.Quality or 0, true)
		end,
	},
	Id = {
		Compare = function(x, y, descending)
			return compareNumbers(x.Id, y.Id, descending)
		end,
	},
}

local a, b = { Rank = 1, Quality = 3, Id = 5 }, { Rank = 2, Id = 4 }
print(sorters.Rank.Compare(a, b), sorters.Quality.Compare(a, b), sorters.Id.Compare(a, b, false), sorters.Id.Compare(a, a, true))
