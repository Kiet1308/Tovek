local function helper(p)
	return (p * 3 + 5) * 2 + 11
end

local function alternative(p)
	return (p * 3 + 5) * 2 + 11
end

return {
	helper = helper,
	alternative = alternative,
	run = function(p, _, _, _)
		return (p * 3 + 5) * 2 + 11, (p * 3 + 5) * 2 + 11
	end
}
