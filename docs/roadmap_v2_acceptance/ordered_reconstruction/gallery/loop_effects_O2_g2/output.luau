local function run(value, flag, sink, tail)
	sink("step", value * 1)
	sink("step", value * 2)
	sink("step", value * 3)
	sink("step", value * 4)
	return value
end

return {
	run = run
}
