-- Every keyed table and every array of more than three elements printed one
-- entry per line, so `{ Asset = "Gem", Amount = 500 }` took four lines. A
-- table of at most 4 keyed or 8 array entries, each atomic (a literal, a
-- name, a field chain, a negated number, an empty table, a short call of
-- atomic arguments), now prints on one line when it ends by column 100.
-- Mixed tables, nested tables and functions keep one entry per line.
local function show(t)
	local keys = {}
	for key in t do
		table.insert(keys, tostring(key))
	end
	table.sort(keys)
	local out = {}
	for _, key in keys do
		local value = t[key] or t[tonumber(key)]
		table.insert(out, key .. "=" .. (type(value) == "table" and "{" .. #value .. "}" or tostring(value)))
	end
	return table.concat(out, " ")
end

local config = { Asset = "Gem", Amount = 500 }
local angles = { 225, 315, 135, 45, -90, 0, 360, 180 }
local id = { ID = config.Asset }
local sized = { Size = math.max(1, 2), Empty = {}, Scale = -0.5 }
local mixed = { 1, 2, x = 3 }
local nested = { inner = { 1, 2 } }
local wide = { Message = "a long message that will not fit on one line with the rest of it", Code = 404 }

print(show(config))
print(show(angles))
print(show(id), show(sized))
print(show(mixed), show(nested), show(wide))
