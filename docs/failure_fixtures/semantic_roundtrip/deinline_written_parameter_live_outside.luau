-- A written parameter stands for the caller local SSA coalesced it into
-- only where that local is dead once the copy ends, and the copy may sit in
-- a nested block: what runs after it is the rest of the block, then the
-- rest of every block around it, and the next trip of every loop around
-- it. `drain`, `once`, `settle`, `drainParts` and `cut` are hand-written
-- shapes of `put` and `norm` whose local stays live outside the block
-- holding them: carried around a loop, read after an `if`, read by a
-- `while` condition. None is a call of the helper: rebuilt, the local
-- would keep the value it had before. Where nothing reads the local after
-- the `if`, the real copy in `quiet` is still the helper's call
-- (`put(9, v)`).
local out = {}

local function put(at, v)
	if v > 100 then
		v = v - 100
	end
	out[at] = tostring(v)
end

local function norm(v)
	if v > 100 then
		v = v - 100
	end
	return "<" .. tostring(v) .. ">"
end

-- `v` is carried around the loop and returned after it.
local function drain(v)
	for i = 1, 3 do
		if v > 100 then
			v = v - 100
		end
		out[i] = tostring(v)
	end
	return v
end

-- `v` is read after the `if`.
local function once(v, go)
	if go then
		if v > 100 then
			v = v - 100
		end
		out[4] = tostring(v)
	end
	return v
end

-- The loop condition reads `v`: rebuilt, the loop would never end.
local function settle(v)
	local n = 0
	while v > 100 do
		n += 1
		if v > 100 then
			v = v - 100
		end
		out[4 + n] = tostring(v)
	end
	return v, n
end

-- The value form, carried around the loop.
local function drainParts(v)
	local parts = {}
	for i = 1, 3 do
		if v > 100 then
			v = v - 100
		end
		local s = "<" .. tostring(v) .. ">"
		parts[i] = s
	end
	return v, table.concat(parts)
end

-- The value form, read after the `if`.
local function cut(v, go)
	local s = "-"
	if go then
		if v > 100 then
			v = v - 100
		end
		s = "<" .. tostring(v) .. ">"
	end
	return v, s
end

-- A real copy: nothing reads `v` after the `if`.
local function quiet(v, go)
	if go then
		put(9, v)
	end
end

put(10, 150)
print(drain(350), out[1], out[2], out[3])
print(once(250, true), out[4], out[10])
print(settle(350))
print(out[5], out[6], out[7])
print(norm(150), drainParts(350))
print(cut(250, true))
print(cut(250, false))
quiet(170, true)
quiet(20, false)
print(out[9])
