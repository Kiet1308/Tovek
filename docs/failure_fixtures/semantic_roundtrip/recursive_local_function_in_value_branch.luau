-- A recursive local function created in one arm of a value branch: its
-- closure captures its own register (NEWCLOSURE fills it before CAPTURE VAL
-- reads it), so that register's write is the binding's only definition.
-- Collapsing the branch into `c and function() ... f ... end or other` moved
-- the closure out of its definition: `f` was printed as a bare `local f` (nil)
-- and the recursion called nil. Shapes: the then-triangle, the
-- else-triangle and the full diamond, at -O0 and -O1.
local function chooseRecursive(flag, fallback)
	local chosen = fallback
	if flag then
		local function countdown(k)
			if k <= 0 then
				return "done"
			end
			return countdown(k - 1)
		end
		chosen = countdown
	end
	return chosen
end
print("T", chooseRecursive(true, print)(3), chooseRecursive(false, function() return "fallback" end)(3))

local function choose(flag)
	local f
	if flag then
		local function rec(n)
			if n == 0 then return "rec" end
			return rec(n - 1)
		end
		f = rec
	else
		f = function() return "plain" end
	end
	return f
end
print("S", choose(true)(3), choose(false)(3))

local function chooseElse(flag, other)
	local f = other
	if not flag then
		local function rec(n)
			if n == 0 then return "rec2" end
			return rec(n - 1)
		end
		f = rec
	end
	return f(2)
end
print("E", chooseElse(false, nil), chooseElse(true, function() return "other" end))

local function pick(flag)
	local f
	if flag then
		local function g(n)
			if n <= 0 then return "g" end
			return g(n - 1)
		end
		f = g
	else
		f = 1
	end
	return f
end
print("G", pick(true)(2), pick(false))
