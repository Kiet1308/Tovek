local function helper(value)
	local product = value * 3
	return (product + 5) * 2 + 11
end

local function run(p, flag, sink, tail)
	local product = p * 3
	local first = (product + 5) * 2 + 11
	local product2 = p * 3
	local second = (product2 + 5) * 2 + 11
	return first, second
end

return {
	helper = helper,
	run = run
}
