local v = 1
print((function(p, p2, p3)
	return p.run(p2, p3)
end)({
	run = function(p, callback)
		if not p then
			return -1
		end

		callback()
		return v
	end
}, true, function()
	v = 99
end))
