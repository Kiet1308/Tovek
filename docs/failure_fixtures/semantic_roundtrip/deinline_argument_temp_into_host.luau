-- The temp of an argument Luau evaluated before a copy that sits inside a
-- larger expression (`math.max(jitter, 0, hashInt(record.Id, 7) % ...)`,
-- `record.Id` read after `jitter` and `0`): every value the statement
-- evaluates before the call is a literal, a constant import or a local no
-- code writes, so the temp goes into the call, where the source had it.
local function hashInt(value, seed)
	local h = seed + 2166136261
	for i = 1, #value do
		h = bit32.bxor(h, string.byte(value, i)) * 16777619 % 4294967296
	end
	return h
end

function place(record, x)
	local jitter = hashInt(record.Id, 3) % 10000 / 10000 - 0.5
	local offset = math.max(jitter, 0, hashInt(record.Id, 7) % 10000 / 10000 - 0.5)
	return offset * x
end

print(place({ Id = "plant-17" }, 2))
