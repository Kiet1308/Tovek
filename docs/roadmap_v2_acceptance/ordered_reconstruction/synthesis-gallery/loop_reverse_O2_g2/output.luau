local function run(value, flag, sink, tail)
	local total = 0
	local total2 = total + value * 4
	local total3 = total2 + value * 3
	local total4 = total3 + value * 2
	local total5 = total4 + value * 1
	return total5
end

return {
	run = run
}
