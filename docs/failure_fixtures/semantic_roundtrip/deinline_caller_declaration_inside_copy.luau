-- A caller's declaration Luau sets inside a copy, where its register is
-- written: the `0` loaded for `math.max(x, 0, ...)` before the copy of
-- `hashInt` runs, and `local tint` declared while the copy's loop is still
-- ahead. Each is tried before the copy, which is then its call.
local function hashInt(value, seed)
	local h = seed + 2166136261
	for i = 1, #value do
		h = bit32.bxor(h, string.byte(value, i)) * 16777619 % 4294967296
	end
	return h
end

function place(id, x)
	local jitter = hashInt(id, 3) % 10000 / 10000 - 0.5
	local offset = math.max(jitter, 0, hashInt(id, 7) % 10000 / 10000 - 0.5)
	return offset * x
end

function shade(key, glowing)
	local base = hashInt(key, 23) % 10000 / 10000
	local tint
	local scale = hashInt(key, 41) % 10000 / 10000 * 0.22 + 1
	if glowing then
		tint = base * scale
	end
	return base, tint, scale
end

print(place("plant-17", 2))
print(shade("pot", true))
print(shade("pot", false))
