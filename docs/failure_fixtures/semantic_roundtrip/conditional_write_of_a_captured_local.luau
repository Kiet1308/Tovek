-- A conditional write of a local some closure captures, folded into a
-- select by the SSA conditional collapse (`structure_bool_conditional`).
-- The collapse drops the arm and binds `c and x or a` to the phi's local.
-- Where the phi is a plain version (here: the capture happens in an earlier
-- `if`, so the versions after it are not all cell versions), the arm's write
-- of the cell went with it: `return p > 0 and 1 or p` left the cell at 5 and
-- `f()` returned 5 instead of 1 (fr-core F2, rtvm 4/18). An arm writing a
-- cell into a plain phi now stays an `if`. The diamond (`else a = 2`) and a
-- loop variant hit the same rule. Negative: `clamp`'s cell is captured
-- before every version, so its phi is a cell version and the select writes
-- the cell itself: still one line.
local f
local function body(a, flag)
	if flag then
		f = function()
			return a
		end
	end
	if a > 0 then
		a = 1
	end
	return a
end
print(body(5, true), f())
print(body(-5, true), f())

local g
local function diamond(a, flag)
	if flag then
		g = function()
			return a
		end
	end
	if a > 0 then
		a = 1
	else
		a = 2
	end
	return a
end
print(diamond(5, true), g())
print(diamond(-5, true), g())

local h = {}
local function looped(n)
	local total = 0
	for i = 1, n do
		if i % 2 == 0 then
			h[#h + 1] = function()
				return total
			end
		end
		if total < i then
			total = i * 10
		end
	end
	return total
end
print(looped(3), h[1]())

local function clamp(value)
	local read = function()
		return value
	end
	if value > 600 then
		value = 600
	end
	return value, read()
end
print(clamp(700))
print(clamp(7))
