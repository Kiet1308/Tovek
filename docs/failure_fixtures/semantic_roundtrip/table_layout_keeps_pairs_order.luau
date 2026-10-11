-- `pairs` visits a table's keys in the order of its hash part's nodes, which
-- depends on how the bytecode built the table. A DUPTABLE copies a template
-- whose keys all exist before any store, in a hash part sized for them
-- (rounded up to a power of two); a NEWTABLE gets a key only when a store of
-- a value creates it. Two constructors printed from them changed that order:
-- `twoThenOne`: a two-key template grown by a later store rehashes; folded
-- into one three-key template it was laid out otherwise (fr-t1110 F2), so
-- the store now stays a statement past the template's nodes (`threeThenOne`
-- has room for a fourth key and still folds). `newTable`: a constructor with
-- a bracketed key compiles to NEWTABLE and stores, which skip `Parent`
-- stored `nil`; printed with record fields only it compiled to a template
-- keeping `Parent`'s node (fr-t1110 F1), so one key prints bracketed again.
-- Both in V2.5.1 too.
local function keys(t)
	local out = {}
	for k in pairs(t) do
		table.insert(out, k)
	end
	return table.concat(out, ",")
end

local function twoThenOne(a, b)
	local entry = { BLlua = a, OAMqFep = b }
	entry.VRTLoAtzT = function()
		return 1
	end
	return entry
end
print(keys(twoThenOne(1, 2)))

local function threeThenOne(a, b, c)
	local entry = { alpha = a, beta = b, gamma = c }
	entry.delta = a + b
	return entry
end
print(keys(threeThenOne(1, 2, 3)))

local function newTable(order, visible)
	return {
		BackgroundColor3 = 1,
		Size = 2,
		SizeConstraint = 3,
		LayoutOrder = order,
		PlaceholderText = "-",
		Text = "",
		TextSize = 15,
		RichText = false,
		Parent = visible and "p" or nil,
		["UICorner"] = { CornerRadius = 4 },
	}
end
print(keys(newTable(1, false)))
print(keys(newTable(2, true)))
