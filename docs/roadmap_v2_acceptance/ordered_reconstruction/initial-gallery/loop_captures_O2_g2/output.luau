local function run(value, flag, sink, tail)
	local callbacks = {}

	for snapshot = 1, 4 do
		local snapshot2 = snapshot

		callbacks[snapshot] = function()
			return snapshot2
		end
	end

	return callbacks[1](), callbacks[2](), callbacks[3](), callbacks[4]()
end

return {
	run = run
}
