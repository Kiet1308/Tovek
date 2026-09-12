local function helper(value, flag)
	if flag then
		local result = (value * 3 + 5) * 2
		return result
	else
		local result = (value * 7 + 4) * 3
		return result
	end
end

local function run(p, p2, sink, tail)
	local result

	if p2 then
		result = (p * 3 + 5) * 2
	else
		result = (p * 7 + 4) * 3
	end

	local first = result
	local result2 = (p * 7 + 4) * 3
	local second = result2
	return first, second
end

return {
	helper = helper,
	run = run
}
