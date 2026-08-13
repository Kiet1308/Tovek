-- Big mixed constant table: positional vs explicit-key collision (positional
-- wins), 2^53 boundary, inf, signed zero, subtable nesting, mixed key types.
local t = {
	"alpha",
	"beta",
	"gamma",
	count = 9007199254740993,
	huge = 1 / 0,
	negzero = -0.0,
	nested = { { { name = "deep" } }, extra = 7 },
	["key with spaces"] = true,
	[3] = "three",
	[0] = "zero",
	[""] = "empty",
}
local keys = {}
for k in pairs(t) do
	keys[#keys + 1] = k
end
table.sort(keys, function(a, b) return tostring(a) < tostring(b) end)
for _, k in ipairs(keys) do
	local v = t[k]
	if type(v) == "table" then
		print(k, "table", v[1][1].name, v.extra)
	else
		print(k, tostring(v))
	end
end
print(#t, t[3], t[0], t.count)
print(t[""], t.alpha, t.beta)