-- A `while true` main loop (no reachable exit) whose nested loop leaves to
-- the outer header. Exit-based post-dominance is empty in such a function;
-- the inner loop must still be recognized. (Reported: residual goto/label.)
local log = {}
local ticks = 0
local function step()
	ticks += 1
	if ticks > 14 then
		error("stop", 0)
	end
	return ticks
end
local function main()
	while true do
		if step() % 2 == 0 then
			while true do
				if step() % 3 == 0 then
					log[#log + 1] = "u" .. ticks
					break
				end
				for i = 1, 2 do
					log[#log + 1] = "i" .. i
				end
				if step() % 5 == 0 then
					break
				end
			end
		end
	end
end
print(pcall(main))
print(table.concat(log, ","))
