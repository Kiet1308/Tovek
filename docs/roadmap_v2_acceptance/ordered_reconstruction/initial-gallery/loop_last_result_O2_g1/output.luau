return {
	run = function(_, _, callback, _)
		callback("index", 1)
		callback("index", 2)
		callback("index", 3)
		local v = 4
		callback("index", v)
		return v
	end
}
