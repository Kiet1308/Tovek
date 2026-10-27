-- Runtime-nil fields must keep NEWTABLE collision placement; DUPTABLE pre-seeds all keys.
local function keys(t)
	local out = {}
	for k, v in pairs(t) do
		table.insert(out, tostring(k) .. "=" .. tostring(v))
	end
	return table.concat(out, ",")
end
local function make(v1, v2, v3, v4, v5, v6, v7, v8)
	return {
		["BackgroundColor3"] = v1,
		Size = v2,
		SizeConstraint = v3,
		LayoutOrder = v4,
		PlaceholderText = v5,
		Text = v6,
		TextSize = v7,
		RichText = v8,
	}
end
print(keys(make(1, 2, 3, 4, 5, 6, 7, 8)))
print(keys(make(nil, 2, 3, 4, 5, 6, 7, 8)))
print(keys(make(1, nil, 3, 4, 5, 6, 7, 8)))
print(keys(make(1, 2, nil, 4, 5, 6, 7, 8)))
print(keys(make(1, 2, 3, nil, 5, 6, 7, 8)))
print(keys(make(1, 2, 3, 4, nil, 6, 7, 8)))
print(keys(make(1, 2, 3, 4, 5, nil, 7, 8)))
print(keys(make(1, 2, 3, 4, 5, 6, nil, 8)))
print(keys(make(1, 2, 3, 4, 5, 6, 7, nil)))

-- If every earlier value is non-nil, the last store keeps the same nodes.
local function lastOnly(value)
	return {
		BackgroundColor3 = 1,
		Size = 2,
		SizeConstraint = 3,
		LayoutOrder = 4,
		PlaceholderText = 5,
		Text = 6,
		TextSize = 7,
		["RichText"] = value,
	}
end
local last = lastOnly(nil)
print(keys(last), next(last, "RichText"))
last.Parent = 9
print(keys(last))
print(keys(lastOnly(false)))
print(keys(lastOnly(8)))
