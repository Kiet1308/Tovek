local function run(value, flag, sink, tail)
	-- equivalent fixed-count loop synthesized; original loop unknown
	local total = 0

	for i = 1, 4 do
		total += value * i
	end

	return total
end

return {
	run = run
}
