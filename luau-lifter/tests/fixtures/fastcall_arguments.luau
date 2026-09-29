local function describe(value, depth)
	local out = {}
	for key, item in pairs(value) do
		table.insert(out, ("  "):rep(depth + 1))
		table.insert(out, tostring(key))
		table.insert(out, string.format("%q", tostring(item)))
	end
	return table.concat(out, "")
end

return describe
