-- Helpers returning a new function (closure factories). Luau's -O2 copy of
-- `bindSelf(self, self.a)` creates the same child prototype in the caller,
-- capturing the caller's values: the de-inliner matches the closure by
-- prototype and captures and rebuilds the call where the closure stood, a
-- field store taking in the temp of the method (`self.a = bindSelf(self,
-- self.a)`), two arguments of one call (`promise:andThen(finalize(resolve),
-- finalize(reject))`), a branch storing into a local (`createAdvancer`).
-- `finalize`'s function reads `cancel` through `finalize`'s own upvalue, its
-- copy straight from the caller's local: the same value, as nothing assigns
-- `cancel` after its declaration. `refresh` rebuilds as a statement for the
-- same reason.
-- Negative: `mk` returns a function capturing nothing, which Luau loads as one
-- shared constant per caller (DUPCLOSURE): its two copies in `T.left` and
-- `T.right` are two objects, which one helper's two calls would make one.
-- They stay. (Luau's own inlining makes them two where the source has one,
-- so only their behavior is printed.)
local function bindSelf(self, method)
	return function(firstArg, ...)
		if firstArg == self then
			return method(self, ...)
		end
		return method(self, firstArg, ...)
	end
end

local E = {}
E.__index = E
function E.a(self, x) return "a" .. tostring(x) end
function E.ok(self, x) return "ok" .. tostring(x) end
function E.new(value)
	local self = setmetatable({ value = value }, E)
	self.a = bindSelf(self, self.a)
	self.ok = bindSelf(self, self.ok)
	return self
end

local function race(promises, onCancel, resolve, reject)
	local newPromises = {}
	local finished = false
	local function cancel()
		for _, p in ipairs(newPromises) do
			p.cancelled = true
		end
	end
	local function finalize(callback)
		return function(...)
			cancel()
			finished = true
			return callback(...)
		end
	end
	if onCancel(finalize(reject)) then
		return
	end
	for i, promise in ipairs(promises) do
		newPromises[i] = promise:andThen(finalize(resolve), finalize(reject))
	end
	return newPromises, finished
end

local function createAdvancer(tag, handler, resolve, reject)
	return function(...)
		local ok, value = pcall(handler, ...)
		if ok then
			resolve(tag, value)
		else
			reject(tag, value)
		end
	end
end

local function andThen(tag, onOk, onErr, resolve, reject)
	local success = resolve
	if onOk then
		success = createAdvancer(tag, onOk, resolve, reject)
	end
	local failure = reject
	if onErr then
		failure = createAdvancer(tag, onErr, resolve, reject)
	end
	return success, failure
end

local log = {}
local function refresh(target)
	table.insert(log, function()
		return target .. #log
	end)
end

local function mk()
	return function() return 1 end
end
local T = {}
function T.left() return mk() end
function T.right() return mk() end

local P = {}
P.__index = P
function P:andThen(a, b) return setmetatable({ a = a, b = b }, P) end

local e = E.new(1)
print(e.a(5), e:a(6), e.ok(7), e:ok(8))
local out, finished = race({ setmetatable({}, P), setmetatable({}, P) }, function() return false end,
	function(...) return "resolved", ... end, function(...) return "rejected", ... end)
print(out[1].a(1, 2), out[2].b(3), out[1].cancelled, finished)
local s, f = andThen("A", function(x) return x * 2 end, function() error("bad", 0) end,
	function(tag, v) print("ok", tag, v) end, function(tag, v) print("err", tag, v) end)
s(21)
f(1)
refresh("x")
refresh("y")
print(log[1](), log[2]())
print(T.left() == T.left(), T.left()(), T.right()())
