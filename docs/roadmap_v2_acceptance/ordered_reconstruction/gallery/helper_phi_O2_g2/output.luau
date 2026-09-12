--  equivalent arithmetic calls inferred from this bytecode helper; original call sites unknown
local function helper(value, flag)
	if flag then
		local result = (value * 3 + 5) * 2
		return result
	else
		local result = (value * 7 + 4) * 3
		return result
	end
end

local function run(value, flag, sink, tail)
	local result = helper(value, flag)
	local first = result
	local result2 = (value * 7 + 4) * 3
	local second = result2
	return first, second
end

return {
	helper = helper,
	run = run
}
