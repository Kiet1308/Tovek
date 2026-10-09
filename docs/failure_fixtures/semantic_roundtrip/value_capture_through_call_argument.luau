-- A closure made inside a call argument captures, by value, a variable that
-- shares a phi with the call's result: `failure = keep(function() reject() end)`
-- after `local failure = reject`. Out-of-SSA coalescing merged `reject` into
-- `failure` and printed `failure = keep(function() failure() end)`, so the
-- closure (by reference in source) called itself until the stack overflowed.
-- No inlining is involved: it failed at -O0, -O1 and -O2. (B) takes the other
-- path; (C) rebinds a loop variable through the same call; (D) reads back
-- what `keep` stored.
local stored = {}
local function keep(f)
	stored[#stored + 1] = f
	return f
end

local function pick(tag)
	return function(reject, sink)
		local failure = reject
		if tag then
			failure = keep(function(x)
				return reject(x) * 100 + tag
			end)
		end
		sink(failure)
	end
end

local function rebind(n)
	local handler = function(x)
		return x
	end
	for i = 1, n do
		local previous = handler
		handler = keep(function(x)
			return previous(x) + i
		end)
	end
	return handler
end

pick(1)(function(x) return x * 10 end, function(b) print("A", b(2)) end)
pick(nil)(function(x) return x * 10 end, function(b) print("B", b(2)) end)
print("C", rebind(3)(100))
print("D", #stored, stored[1](5))
