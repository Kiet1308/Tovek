-- Value helpers called for no result. Luau's -O2 copy evaluates each
-- `return X` only for its effects (`compileExprSide`): nothing for a local,
-- a global, a constant or a function literal, and the call itself for a
-- call. The de-inliner matches such sites against the helper's discard body
-- and rebuilds a call statement (`track(spawnTask(f))`), taking in the temp
-- Luau made for an argument the body reads twice. Where the caller keeps
-- reading that temp, the call declares it instead (`local t = track(...)`),
-- exact because every path of `track` returns that parameter.
-- `peek` returns `t.count`, an index Luau still evaluates for its effects:
-- it has no discard body; its copy keeps that value in an unused local, and
-- the call declares it. `describe("label")` folds away completely: Luau
-- evaluates `type("label")` at compile time.
local log = {}
local settings = {}
local function track(x)
	if type(x) == "table" then
		table.insert(log, x)
	end
	return x
end

local function describe(x)
	if type(x) == "number" then
		table.insert(log, { n = x })
		return tostring(x * 2)
	end
	return x
end

local function remember(key, value)
	settings[key] = value
	table.insert(log, { key = key })
	return VERSION
end

local function peek(t)
	table.insert(log, { peeked = t })
	return t.count
end

local function spawnTask(name)
	return { name = name, count = #log }
end

local function run(n)
	track(spawnTask("first"))
	if n > 1 then
		track(spawnTask("second"))
	else
		track(n)
	end
	describe(n)
	describe("label")
	remember("k", n)
	peek({ count = n })
	local kept = track(spawnTask("kept"))
	return kept.name, kept.count, #log
end

print(run(2))
print(run(1))
local names = {}
for _, entry in log do
	table.insert(names, entry.name or entry.key or (entry.n and "n" .. entry.n) or (entry.peeked and "peek") or "?")
end
print(table.concat(names, ","), settings.k)
