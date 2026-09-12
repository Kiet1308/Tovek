local function run(value, flag, sink, tail)
	local total = 0

	local function read()
		return total
	end

	for index = 1, 4 do
		local v = value * index
		total += v
		sink("observed", total)
	end

	return total
end

return {
	run = run
}
