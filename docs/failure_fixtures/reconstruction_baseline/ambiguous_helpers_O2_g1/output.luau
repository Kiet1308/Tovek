local AmbiguousHelpersO2G1 = {}

function AmbiguousHelpersO2G1.helper(p)
	return (p * 3 + 5) * 2 + 11
end

function AmbiguousHelpersO2G1.alternative(p)
	return (p * 3 + 5) * 2 + 11
end

function AmbiguousHelpersO2G1.run(p, _, _, _)
	return (p * 3 + 5) * 2 + 11, (p * 3 + 5) * 2 + 11
end

return AmbiguousHelpersO2G1
