-- A generic `for` whose builtin callee is a local alias written once in an
-- earlier block (`local pairs = pairs` before an earlier loop). Luau emits the
-- FORGPREP_NEXT fast path only for proven builtin aliases.
-- (Real dump: Roblox RbxCharacterSounds failed to structure.)
local function count(t, u)
	local pairs = pairs
	local n = 0
	for _ in pairs(t) do
		n += 1
	end
	local seen = {}
	for k in pairs(u) do
		seen[#seen + 1] = k
	end
	table.sort(seen)
	return n, table.concat(seen, ",")
end
print(count({ a = 1, b = 2 }, { x = 1, y = 2, z = 3 }))
