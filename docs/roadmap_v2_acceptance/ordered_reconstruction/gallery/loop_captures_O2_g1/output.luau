return {
	run = function(_, _, _, _)
		local v = {}

		for i = 1, 4 do
			local v2 = i

			v[i] = function()
				return v2
			end
		end

		return v[1](), v[2](), v[3](), v[4]()
	end
}
