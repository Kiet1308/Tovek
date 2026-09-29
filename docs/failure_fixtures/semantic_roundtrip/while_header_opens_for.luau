-- A `while true` whose first block also prepares a nested `for`: the lifter
-- keeps the loop-opening code and the FORGPREP in the while header block.
-- (Real dump: SkyGarden pulse-fade/Krixi FX/Disco failed to structure.)
local function fade(parts, lights, frames)
	local start = 0
	local log = {}
	while true do
		start += 1
		local alpha = math.clamp(start / frames, 0, 1)
		for _, part in parts do
			if part.alive then
				part.value = part.base + (1 - part.base) * alpha
			end
		end
		for _, light in lights do
			if light.alive then
				light.value = light.base * (1 - alpha)
			end
		end
		log[#log + 1] = string.format("%.2f", alpha)
		if alpha >= 1 then
			return table.concat(log, " ")
		end
	end
end
print(fade({ { alive = true, base = 0.5 }, { alive = false, base = 0 } }, { { alive = true, base = 2 } }, 4))
