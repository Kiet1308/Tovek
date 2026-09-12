--  equivalent arithmetic calls inferred from this bytecode helper; original call sites unknown
local function helper(p)
	return (p * 3 + 5) * 2 + 11
end

return {
	helper = helper,
	run = function(p, _, _, _)
		return helper(p), helper(p)
	end
}
