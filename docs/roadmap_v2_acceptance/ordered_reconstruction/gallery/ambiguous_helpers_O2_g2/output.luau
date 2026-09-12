local function helper(value)
	return (value * 3 + 5) * 2 + 11
end

local function alternative(value)
	return (value * 3 + 5) * 2 + 11
end

local function run(p, flag, sink, tail)
	return (p * 3 + 5) * 2 + 11, (p * 3 + 5) * 2 + 11
end

return {
	helper = helper,
	alternative = alternative,
	run = run
}
