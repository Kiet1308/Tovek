--  equivalent arithmetic calls inferred from this bytecode helper; original call sites unknown
local function helper(value)
	return (value * 3 + 5) * 2 + 11
end

local function run(value, flag, sink, tail)
	local first = helper(value)
	local second = helper(value)
	return first, second
end

return {
	helper = helper,
	run = run
}
