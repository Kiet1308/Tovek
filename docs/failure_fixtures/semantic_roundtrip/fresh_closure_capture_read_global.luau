-- A literal a function makes anew (NEWCLOSURE) whose captured constant Luau
-- folded out of every read (`if DEBUG then` disappears at -O1 -g2): the
-- output gives it `local _ = DEBUG` first, a read that keeps the capture and
-- with it a new closure per run (`fresh_closures`). The body here reads and
-- writes a GLOBAL `_`: `return _` and the `_ = v` discard idiom. A local
-- named `_` hid that global, so `get()` returned `false` (the folded flag)
-- and `put(1)` printed `nil` (the write went to the local). The read is now
-- named past every name the body spells, globals included (`_2`).
-- At -g0/-g1 the constant folds with no capture and nothing is inserted.
_ = "global underscore"
local function make()
	local DEBUG = false
	return function()
		if DEBUG then
			print("dbg")
		end
		return _
	end
end
local a, b = make(), make()
print(a == b, a(), b())

local function writer()
	local DEBUG = false
	return function(v)
		if DEBUG then
			print("dbg")
		end
		_ = v
		print(_)
	end
end
local put1, put2 = writer(), writer()
put1(1)
put2(2)
print(put1 == put2, _)
