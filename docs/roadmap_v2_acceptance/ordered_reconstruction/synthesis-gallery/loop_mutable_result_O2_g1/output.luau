return {
	run = function(p, _, callback, _)
		local total = 0

		local function read()
			return total
		end

		for i = 1, 4 do
			local v = p * i
			total += v
			callback("observed", total)
		end

		return total
	end
}
