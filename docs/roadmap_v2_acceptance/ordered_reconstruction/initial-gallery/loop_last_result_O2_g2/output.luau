local function run(value, flag, sink, tail)
	local last = 1
	sink("index", last)
	local last2 = 2
	sink("index", last2)
	local last3 = 3
	sink("index", last3)
	local last4 = 4
	sink("index", last4)
	return last4
end

return {
	run = run
}
