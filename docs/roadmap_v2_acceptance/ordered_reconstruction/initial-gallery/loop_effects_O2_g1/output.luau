return {
	run = function(p, _, callback, _)
		callback("step", p * 1)
		callback("step", p * 2)
		callback("step", p * 3)
		callback("step", p * 4)
		return p
	end
}
