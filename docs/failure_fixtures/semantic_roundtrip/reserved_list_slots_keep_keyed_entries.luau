-- A constructor split before its list items reserves their slots with
-- `nil` items, and a `nil` item stores after every keyed entry. A keyed
-- entry in a reserved slot (`[2]`) survives in the bytecode when the list
-- stores leave its slot alone: a trailing call returning nothing (`multi(0)`)
-- stores only the items before it. The reservation stops below such an
-- entry. Found by the typed fuzzer.
local function build(x, a)
	local function multi(count)
		print("multi", count)
		return table.unpack({ a, 2, 3 }, 1, count)
	end
	local t = { [2] = (0.5 < 3) and x or a, [0] = nil, (multi(1) or a), multi(0) }
	a = nil
	return t[1], t[2], t[3]
end
print(build("x", "a"))

local function overwritten(x, a)
	local function multi(count)
		print("multi", count)
		return table.unpack({ a, 2, 3 }, 1, count)
	end
	local t = { [1] = x, [3] = x, (multi(1) or a), multi(2) }
	a = nil
	return t[1], t[2], t[3], t[4]
end
print(overwritten("x", "a"))
