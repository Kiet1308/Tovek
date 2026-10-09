-- Helpers whose result the caller stores back into a local it already has
-- (`buf = grow(buf, n, state)`, `top = better(top, item)`). Luau's -O2 copy writes
-- that local only at the end of each path; on the path returning the
-- argument itself (`return source`) it writes nothing at all, a register
-- copied into itself. The de-inliner rebuilds `R = f(...)`: the empty path
-- matches the helper's `return source` only with that parameter pre-bound to
-- the result local, and the argument may read the result local because the
-- leaves' final stores are its only writes. `grow(buf, ...)` also takes in the
-- temp Luau made for `size` (read twice), and `scale` the temp of its second
-- argument.
local function grow(source, size, state)
	if not (state.capacity < size) then
		return source
	end
	local newSize = size * 2
	state.capacity = newSize
	local out = table.create(newSize, 0)
	table.move(source, 1, #source, 1, out)
	return out
end

local function better(current, candidate)
	if candidate.score > current.score then
		return candidate
	end
	return current
end

local function scale(v, f)
	return { x = v.x * f.x, y = v.y * f.y }
end

local function fill(count)
	local state = { capacity = 1 }
	local buf = {}
	local grown = 0
	for i = 1, count do
		local before = buf
		buf = grow(buf, i + 1, state)
		if buf ~= before then
			grown += 1
		end
		buf[i] = i * i
	end
	buf = grow(buf, count + 8, state)
	return #buf, state.capacity, grown, buf[2]
end

local function best(list)
	local top = { name = "none", score = -math.huge }
	for _, item in list do
		top = better(top, item)
	end
	return top.name, top.score
end

local function tint(c, n)
	for i = 1, n do
		c = scale(c, { x = 0.5, y = i })
	end
	return c.x, c.y
end

-- The result stored into another local than the argument: every leaf has
-- its store, nothing is elided.
local function copyInto(list)
	local state = { capacity = 0 }
	local other = grow(list, #list + 2, state)
	local target = list
	target = grow(other, 1, state)
	return target == other, #target, state.capacity
end

-- A callback the copy runs rewrites the result local: the copy read the
-- argument into a temp first, and so does the rebuilt call.
local function apply(value, f)
	if f(value) then
		return value
	end
	return value .. "!"
end

local function rewritten()
	local current = "a"
	local function poke(seen)
		current = "poked:" .. seen
		return false
	end
	current = apply(current, poke)
	return current
end

print(fill(5))
print(fill(0))
print(best({ { name = "a", score = 4 }, { name = "b", score = 9 }, { name = "c", score = 9 } }))
print(best({}))
print(tint({ x = 8, y = 1 }, 3))
print(copyInto({ 1, 2, 3 }))
print(rewritten())
