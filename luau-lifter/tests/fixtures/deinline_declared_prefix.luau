local function clampDepthValue(value)
	local n = tonumber(value)
	if not n then
		return 4
	end
	return math.clamp(math.floor(n), 1, 32)
end

return function(config, item, event)
	local a = clampDepthValue(config:GetAttribute("ChainDepthLimit"))
	event:SetAttribute("ChainDepthLimit", clampDepthValue(item.ChainDepthLimit))
	local b = clampDepthValue(a)
	if a ~= b then
		event:SetAttribute("X", b)
	end
	return a, b
end
