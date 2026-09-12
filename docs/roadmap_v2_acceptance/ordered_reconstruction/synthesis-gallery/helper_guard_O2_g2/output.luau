--  equivalent arithmetic calls inferred from this bytecode helper; original call sites unknown
local function helper(value, flag)
	if flag then
		return (value * 3 + 5) * 2
	else
		return (value * 7 + 4) * 3
	end
end

local function run(value, flag, sink, tail)
	local first = helper(value, flag)
	local second = (value * 7 + 4) * 3
	return first, second
end

return {
	helper = helper,
	run = run
}
