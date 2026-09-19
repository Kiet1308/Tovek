local HelperPhiO2G1 = {}

function HelperPhiO2G1.helper(p, p2)
	if p2 then
		return (p * 3 + 5) * 2
	else
		return (p * 7 + 4) * 3
	end
end

function HelperPhiO2G1.run(p, p2, _, _)
	local v

	if p2 then
		v = (p * 3 + 5) * 2
	else
		v = (p * 7 + 4) * 3
	end

	return v, (p * 7 + 4) * 3
end

return HelperPhiO2G1
