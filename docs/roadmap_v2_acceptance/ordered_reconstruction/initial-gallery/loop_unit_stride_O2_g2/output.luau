local function run(value, flag, sink, tail)
	local total = 0
	local total2 = total + value * 1
	local total3 = total2 + value * 2
	local total4 = total3 + value * 3
	local total5 = total4 + value * 4
	return total5
end

return {
	run = run
}
