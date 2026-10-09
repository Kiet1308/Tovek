-- A closure captures a value by VALUE (CAPTURE VAL) and is assigned to a
-- variable that out-of-SSA coalescing had merged with that captured value.
-- Luau's -O2 inliner makes this shape from `failure = wrap(reject)`:
-- NEWCLOSURE writes `failure`'s register, CAPTURE VAL reads `reject`'s.
-- Printed as `fn = function() ... fn ... end`, the closure (by reference in
-- source) reads itself and recurses until the stack overflows. The real case
-- is Promise._andThen with createAdvancer inlined.
local function wrap(tag, callback)
	return function(x)
		return callback(x) + tag
	end
end

-- (A) branch: `failure = reject` merged into the parameter
local function pick(tag)
	return function(reject, sink)
		local failure = reject
		if tag then
			failure = wrap(tag, reject)
		end
		sink(failure)
	end
end
pick(1)(function(x) return x * 10 end, function(b) print("A", b(2)) end)
pick(false)(function(x) return x * 10 end, function(b) print("A", b(2)) end)

-- (B) Promise._andThen: two advancers; the first must also not snapshot a
-- variable that only the second (wrongly merged) write made "mutated"
local function createAdvancer(tag, handler, resolve, reject)
	return function(...)
		local ok, result = pcall(handler, ...)
		if ok then
			resolve(result)
		else
			reject(tag, result)
		end
	end
end
local function build(successHandler, failureHandler)
	return function(resolve, reject)
		local successCallback = resolve
		if successHandler then
			successCallback = createAdvancer("s", successHandler, resolve, reject)
		end
		local failureCallback = reject
		if failureHandler then
			failureCallback = createAdvancer("f", failureHandler, resolve, reject)
		end
		return successCallback, failureCallback
	end
end
local calls = 0
local onSuccess, onFailure = build(function(v)
	error("bad " .. v, 0)
end, function(e)
	calls += 1
	if calls > 20 then
		return "stop"
	end
	error("boom", 0)
end)(function(v) print("B resolved", v) end, function(tag, e) print("B rejected", tag, e) end)
onSuccess("x")
onFailure("y")
print("B handler calls", calls)

-- (C) loops: `acc = wrap(i, acc)` rebinds the variable it captures; C6's
-- self-capture rewrite (`local own; own = function() own() end`) recursed
local function chain(n)
	local acc = function(x) return x end
	for i = 1, n do
		acc = wrap(i, acc)
	end
	return acc
end
local function chainWhile(n)
	local acc = function(x) return x end
	local i = 0
	while i < n do
		i += 1
		acc = wrap(i, acc)
	end
	return acc
end
print("C", chain(3)(100), chainWhile(3)(100))

-- (D) two variables meet on one edge (a parallel copy); the closure
-- captures the value the other variable is overwritten with
local function make(v)
	return function()
		return v
	end
end
local function pair(a, b, flag)
	local x, y = a, b
	if flag then
		x = make(b)
		y = a
	end
	return x, y
end
local x, y = pair("a", "b", true)
print("D", x(), y, pair("a", "b", false))

-- (E) the closure has a second use, so it stays a statement and a separate
-- phi copy carries it: `d = function() ... reject ... end; failure = d`
local function pickStored(tag)
	return function(reject, sink, store)
		local failure = reject
		if tag then
			failure = wrap(tag, reject)
			store.last = failure
		end
		sink(failure)
	end
end
local store = {}
pickStored(1)(function(v) return v * 10 end, function(b) print("E", b(2)) end, store)
print("E", store.last(3))
