-- The smallest form of a by-value capture merged into the variable its
-- closure is assigned to. Luau -O2 inlines `wrap`: NEWCLOSURE writes
-- `failure`'s register, then CAPTURE VAL reads `reject`'s. Out-of-SSA
-- coalescing put `reject` and `failure` in one variable, printed as
-- `failure = function(x) return failure(x) + tag end`: the closure called
-- itself until the stack overflowed instead of calling `reject`.
local function wrap(tag, callback)
	return function(x)
		return callback(x) + tag
	end
end

local function pick(tag)
	return function(reject, sink)
		local failure = reject
		if tag then
			failure = wrap(tag, reject)
		end
		sink(failure)
	end
end

pick(1)(function(x) return x * 10 end, function(b) print(b(2)) end)
pick(false)(function(x) return x * 10 end, function(b) print(b(2)) end)
return pick
