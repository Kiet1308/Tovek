local function run(p, _, _, _)
	-- equivalent fixed-count loop synthesized; original loop unknown
	local total = 0

	for i = 1, 4 do
		total += p * i
	end

	return total
end

return {
	run = run
}
