--  equivalent arithmetic calls inferred from this bytecode helper; original call sites unknown
local function helper(p, p2)
	if p2 then
		return (p * 3 + 5) * 2
	else
		return (p * 7 + 4) * 3
	end
end

return {
	helper = helper,
	run = function(p, p2, _, _)
		local v = helper(p, p2)
		return v, (p * 7 + 4) * 3
	end
}
