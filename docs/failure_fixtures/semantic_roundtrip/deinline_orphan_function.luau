-- An anonymous function only ever called (`local scramble = function`),
-- all of whose calls Luau -O2 inlined: its declaration is dead, and as it
-- has no name the SSA inliner deleted it, so its copies stayed pasted. The
-- declaration is now kept aside (it counts as no use of anything, so the
-- rest of the program decompiles as before) and goes back, right before
-- the first statement calling it, once a copy is rebuilt as a call:
-- `v = fn(v, key)` in `writeb`.
-- Negatives: `shift` captures `level`, so it is no orphan and its copy
-- stays; `folded`'s only copy folded to a constant: it stays aside, unseen.
local scramble = function(v, key)
	if key then
		return (v + math.floor(v / 2)) % 128
	end
	return v
end
local function writeb(buf, offset, v, key)
	v = scramble(v, key)
	buf[offset] = v
end
local level = tonumber("2")
local shift = function(v)
	return math.max(v * level, 1)
end
local function shifted(t)
	t.x = shift(t.x)
end
local folded = function(v)
	if v > 10 then
		return math.min(v - 10, 5)
	end
	return v
end
local buf = {}
writeb(buf, 1, 5, true)
writeb(buf, 2, 6, false)
local t = { x = 3 }
shifted(t)
print(buf[1], buf[2], t.x, folded(4))
