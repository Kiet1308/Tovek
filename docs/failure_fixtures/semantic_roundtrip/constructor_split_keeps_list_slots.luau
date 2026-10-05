-- A conditional value in a constructor after an entry that may raise
-- (`b ^ a`): the branch cannot move before the constructor, so the
-- constructor stops there and its list items are stored by statements.
-- Luau sized the array part from those items, so they keep their slots:
-- the constructor reserves them (`nil` items) for the stores to fill.
-- Stores before a SETLIST were entries of the constructor itself and fold
-- back into it, a listed slot's other spelling included (`[0]`, `[-0]`).
-- Found by the typed fuzzer.
local function split(a, b, input)
	local t = { [0] = b ^ a, [1] = (if 255 < b then input else a), nil, 0.5 }
	local count = 0
	for key in next, t do
		count += 1
	end
	return #t, t[0], t[1], t[2], count
end
print(split(1, 2, 3))
print(split(1, 300, 3))

local function respelled(flip, a, cell)
	local t = { [0] = nil, x = 1, [-0] = ("%*-%*"):format(a, cell), k = (cell < 256), tostring(flip), nil, math.floor(cell) }
	t[0] = 0
	t[-0] = math.floor(a)
	for key, value in pairs(t) do
		if type(key) == "number" and key == 0 then
			print(1 / key, value)
		end
	end
	return t[1], t[3], t.k
end
print(respelled(true, -3, 2))
