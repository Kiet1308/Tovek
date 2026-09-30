-- Helper calls rebuilt around an argument that runs code: each body reads
-- that parameter first and once, so evaluating the argument before the body
-- keeps the order the inlined copy had. `late` reads its parameter after a
-- call, and its sites must keep that order too.
local log = {}
local T = {}
function T.trace(tag, value)
	table.insert(log, tag)
	return value
end

local function toCurve(t)
	return (1 + 0.1) * (2 * math.abs(t) - 1) - 0.1
end

local function warp(h)
	return (h % 1 + 1) % 1
end

local function shift(h, s)
	return warp(h + (s - 0.5) * 1)
end

local size = 2
local function scale(distance, base)
	return base * (math.clamp(distance - 10, 0, 90) / 90 * 1.5 + 1)
end

local function late(t)
	return T.trace("body", 1) + t
end

print(toCurve(math.abs(T.trace("abs", -0.25))))
print(shift(T.trace("h", 0.75), 0.9), warp(T.trace("w", 0.3) + 0.9))
print(scale(T.trace("distance", 55), size))
size = 4
print(scale(40, size))
print(late(T.trace("arg", 4)))
print(table.concat(log, ","))
