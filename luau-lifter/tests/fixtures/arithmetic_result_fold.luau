local function inverse(x: number): number
	if x < 0.0031308 then
		return x * 12.92
	end
	return 1.055 * x ^ (1 / 2.4) - 0.055
end

local function outBounce(t: number): number
	if t < 0.5 then
		return t * t
	end
	return 1 - t
end

return {
	fromLinear = function(linear: Color3): Color3
		return Color3.new(inverse(linear.R), inverse(linear.G), inverse(linear.B))
	end,
	inOutBounce = function(t: number): number
		return outBounce(2 * t - 1) / 2 + 0.5
	end,
}
