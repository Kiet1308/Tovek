local function scale(weight, average, maxScale)
	local maxSize = average * maxScale
	if maxSize <= average then
		return 1
	end
	local t = (weight - average) / (maxSize - average)
	t = math.clamp(t, 0, 1)
	return 1 + t * (maxScale - 1)
end

local function isHidden(instance, inside)
	if inside(instance) then
		return true
	end
	return false
end

local function isSmall(size)
	if size < 1 then
		return true
	else
		return false
	end
end

return { scale = scale, isHidden = isHidden, isSmall = isSmall }
