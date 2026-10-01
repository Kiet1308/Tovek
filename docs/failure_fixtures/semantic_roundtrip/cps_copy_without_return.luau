-- A helper's loop `return` inlines as the caller's continuation copied into
-- the loop, ending in that continuation's own `return`. A copy that falls
-- through instead (no `return`) keeps looping, so this `caller` is not
-- `helper(); print("after"); return`.
stop = true
local function helper()
	for i = 1, 2 do
		if stop then
			return
		end
	end
	print("normal")
end

local function caller()
	for i = 1, 2 do
		if stop then
			print("after")
		end
	end
	print("normal")
	print("after")
	return
end

caller()
stop = false
helper()
