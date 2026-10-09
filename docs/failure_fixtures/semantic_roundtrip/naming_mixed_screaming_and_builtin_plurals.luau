-- Two spelling slips of the derived-name rules.
-- 1. A field name that opens with a SCREAMING segment keeps its spelling:
--    `self._EXTREMELY_DANGEROUS_usedAsValue` names its local
--    `EXTREMELY_DANGEROUS_usedAsValue` (private `_` dropped), not the hybrid
--    `extremely_DANGEROUS_usedAsValue` that lowercasing only the first word
--    invented (Fusion's Spring, Stopwatch and Tween).
-- 2. A builtin's spelling is avoided by the binding that would take it
--    (`local kind = row.Type`), not by the names composed from the word: a
--    list of `.Type` values is `types`, which no builtin spells, not `kinds`.
--    A local aliasing the global `type` keeps the global's spelling.
local function step(self)
	local old = self._EXTREMELY_DANGEROUS_usedAsValue
	self._EXTREMELY_DANGEROUS_usedAsValue = self._goal
	local limit = self._MAX_retryCount
	return old ~= self._goal, limit
end

local function rows(data)
	local list = {}
	for k, v in data.Entries do
		list[k] = v.Type
	end
	return list
end

local function label(row)
	local t = row.Type
	return t .. ":" .. type(t)
end

local type = type
local function check(value)
	return type(value)
end

print(step({ _EXTREMELY_DANGEROUS_usedAsValue = 1, _goal = 2, _MAX_retryCount = 3 }))
print(table.concat(rows({ Entries = { { Type = "a" }, { Type = "b" } } }), ","))
print(label({ Type = "fire" }), check(1), check("s"))
