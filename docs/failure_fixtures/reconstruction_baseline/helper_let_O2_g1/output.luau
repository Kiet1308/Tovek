local HelperLetO2G1 = {}

function HelperLetO2G1.helper(p)
	return (p * 3 + 5) * 2 + 11
end

function HelperLetO2G1.run(p, _, _, _)
	return (p * 3 + 5) * 2 + 11, (p * 3 + 5) * 2 + 11
end

return HelperLetO2G1
