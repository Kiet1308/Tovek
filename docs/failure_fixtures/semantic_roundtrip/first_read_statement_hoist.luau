-- Statement helpers rebuilt around an argument that runs code or reads a
-- captured local: each body reads that parameter first, so the call keeps
-- the order the inlined copy had. `record` stores through a local address
-- before reading its value; `late` reads its parameter after a call.
local log = {}
local T = {}
function T.trace(tag, value)
	table.insert(log, tag)
	return value
end

local function clampCurve(t)
	t = math.clamp(t, -1, 1)
	if t >= 0 then
		return (0.35 * t) / (0.35 - t + 1)
	end
	return -((0.8 * -t) / (0.8 + t + 1))
end

local cache = {}
local function record(key, value)
	cache[key] = value
	table.insert(log, "store")
end

local function late(value)
	table.insert(log, "late")
	cache.late = value
end

local total = 0
local function step(frame)
	table.insert(log, frame / 2)
	total += 1
end
local function bump()
	total += 10
end

print(clampCurve(T.trace("curve", 0.5)), clampCurve(T.trace("curve", -3)))
record("a", T.trace("value", 1))
late(T.trace("arg", 2))
for i = 1, 3 do
	total += i
	step(total)
	bump()
end
print(total, cache.a, cache.late)
print(table.concat(log, ","))
