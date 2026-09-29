local function scale(weight, average, maxScale)
	local maxSize = average * maxScale
	if maxSize <= average then
		return 1
	end
	local t = (weight - average) / (maxSize - average)
	t = math.clamp(t, 0, 1)
	return 1 + t * (maxScale - 1)
end
return scale
