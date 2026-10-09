-- When the name a binding derives from a private field is taken where it
-- lives, the binding takes the field's own spelling before a counter: the
-- field `_getRootData` read inside the function `getRootData` names its local
-- `_getRootData` again (the private `_` is dropped only while the plain name
-- is free), not `getRootData2`; `self._light` beside a visible `light` reads
-- `_light`. Other collisions keep counting: a second CFrame beside a
-- `cframe` is `cframe2`, never the case-only `cFrame` the spelling before
-- acronym-aware lowerCamel gave. Names only: what runs is unchanged.
local function getRootData(data)
	local getter = data._getRootData
	if getter then
		return getter()
	end
	return data._rootData
end

local function apply(self, config)
	local light = config.Light
	local own = self._light
	own.Range = light.Range or config.Range
	own.Color = light.Color
	return own.Range, own.Color
end

local function pivot(part, target)
	local a = part.CFrame
	local b = target.CFrame
	return a.X + b.X, a.Y * b.Y
end

print(getRootData({ _rootData = "root" }), getRootData({ _getRootData = function() return "got" end }))
print(apply({ _light = {} }, { Light = { Color = "red" }, Range = 9 }))
print(pivot({ CFrame = { X = 1, Y = 2 } }, { CFrame = { X = 3, Y = 4 } }))
