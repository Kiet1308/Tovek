-- A list whose elements span lines printed them glued together,
-- `}), New("UICorner")({`, the closing braces out of line. Two or more
-- elements now go one per line once any of them spans lines; a single
-- element keeps hugging its braces (`{ New("UIPadding")({`). `New` is
-- global, so -O2 keeps its calls.
function New(className)
	return function(props)
		props.ClassName = className
		return props
	end
end

local function describe(list)
	local parts = {}
	for i, item in list do
		local keys = {}
		for key in item do
			table.insert(keys, key)
		end
		table.sort(keys)
		local fields = {}
		for _, key in keys do
			table.insert(fields, key .. "=" .. tostring(item[key]))
		end
		parts[i] = table.concat(fields, ",")
	end
	return table.concat(parts, " | ")
end

local children = {
	New("UIPadding")({
		PaddingLeft = 4,
		PaddingRight = 4,
		PaddingTop = 2,
		PaddingBottom = 2,
		Name = "Padding",
	}),
	New("UICorner")({
		CornerRadius = 8,
		Name = "Corner",
		Visible = true,
		ZIndex = 2,
		Parent = "Frame",
	}),
}
local single = {
	New("UIStroke")({
		Thickness = 1,
		Color = "white",
		Transparency = 0.5,
		Name = "Stroke",
		Enabled = true,
	}),
}
print(describe(children))
print(describe(single))
