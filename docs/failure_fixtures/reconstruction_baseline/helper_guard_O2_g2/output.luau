local function helper(value, flag)
	if flag then
		return (value * 3 + 5) * 2
	else
		return (value * 7 + 4) * 3
	end
end

local function run(p, p2, sink, tail)
	local first

	if p2 then
		first = (p * 3 + 5) * 2
	else
		first = (p * 7 + 4) * 3
	end

	local second = (p * 7 + 4) * 3
	return first, second
end

return {
	helper = helper,
	run = run
}
