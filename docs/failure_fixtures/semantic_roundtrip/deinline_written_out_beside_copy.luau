-- A copy Luau -O2 made of a local helper beside the same arithmetic written
-- out: line info tells them apart (the copy keeps the helper's lines), so
-- only the copy becomes a call; and a local the call updates in place
-- (`wavelength = lerp(wavelength, ...)`) keeps the name its first value gave.
local function lerp(a, b, t)
	return a + (b - a) * t
end

local function wave(config, growth, scale)
	local startAmplitude = config.StartAmplitude or 0
	local amplitude = (startAmplitude + ((config.Amplitude or 0) - startAmplitude) * growth) * scale
	local wavelength = config.Wavelength or 12
	if config.EndWavelength then
		wavelength = lerp(wavelength, config.EndWavelength, growth)
	end
	return amplitude, wavelength
end

print(wave({ StartAmplitude = 1, Amplitude = 3, Wavelength = 10, EndWavelength = 20 }, 0.5, 2))
print(wave({ Amplitude = 1 }, 0.25, 1))
