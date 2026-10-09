-- Promise:andThen's shape, with createAdvancer inlined by -O2 into the
-- executor passed to `run`: each callback starts as the plain resolve or
-- reject and is replaced by an advancer closure capturing both. The failure
-- branch's write was coalesced with `reject`, so the failure advancer called
-- itself (stack overflow), and a spurious snapshot appeared in the success
-- branch, which only that wrong write made look mutated.
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

local function andThen(tag, onOk, onErr, run)
	return run(function(resolve, reject)
		local successCallback = resolve
		if onOk then
			successCallback = createAdvancer(tag, onOk, resolve, reject)
		end
		local failureCallback = reject
		if onErr then
			failureCallback = createAdvancer(tag, onErr, resolve, reject)
		end
		return successCallback, failureCallback
	end)
end

local function run(executor)
	local log = {}
	local s, f = executor(function(tag, v) table.insert(log, "ok:" .. tag .. ":" .. tostring(v)) end,
		function(tag, v) table.insert(log, "err:" .. tag .. ":" .. tostring(v)) end)
	return s, f, log
end

local s, f, log = andThen("A", function(x) return x * 2 end, function(x) error("bad", 0) end, run)
s(21)
f(1)
local s2, f2, log2 = andThen("B", nil, nil, run)
s2(5)
f2(6)
print(table.concat(log, ","), table.concat(log2, ","))
