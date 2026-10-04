-- `debug.info(1, "f")` hands out the running function itself, so `saved()`
-- may be `change`, which writes `x`: no call can be shown not to change it.
-- (`...` keeps the compiler from inlining `change`, whose copy would hand
-- out the chunk instead.)
local x = 1
local saved

local function change(...)
	saved = debug.info(1, "f")
	x = 2
	return 0
end

change()
change()
x = 1

local function helper()
	print("value", x + saved())
end

print("value", x + saved())
return helper
