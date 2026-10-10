-- Helpers below the de-inliner's readability floors (fewer than two
-- anchors, or a call no smaller than the copy) whose copies line info shows
-- (plan E1). Luau -O2 keeps a callee's own lines on the code it inlines, so
-- the lifter counts each function's copies of each helper; where the exact
-- matches in a function are exactly those copies, all of them are rebuilt,
-- else none: `disconnect(self.a)` x3 (one anchor), `isEmpty(self.queue)` in
-- a condition, `lerp(a, b, t)` in values (no anchor), `det3(...)` (nine
-- parameters, no anchor), `refresh(node, parent, children)` (a wrapper: one
-- call of another helper, which only the count tells from a direct call),
-- `running()` (a call as big as the copy).
-- Negatives: `close` holds a hand-written `if c then c:Disconnect() end`
-- and no copy of `disconnect`: it stays as written. `blend` holds two copies
-- of `lerp` and one hand-written `a + (b - a) * t`: three matches for two
-- copies, so all three stay. `bump` holds one copy of `inc` and a
-- hand-written `m + 1`: two matches for one copy, both stay. `direct` calls
-- `update` by hand beside one copy of `refresh`: both stay.
-- Without line info (-g0) every copy stays as it was before.
local log = {}
local function conn(name)
	return { Disconnect = function() table.insert(log, "off " .. name) end }
end

local function disconnect(c)
	if c then
		c:Disconnect()
	end
end

local function isEmpty(t)
	return next(t) == nil
end

local function lerp(a, b, t)
	return a + (b - a) * t
end

local function det3(a, b, c, d, e, f, g, h, i)
	return a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g)
end

local function update(node, parent, children)
	for _, child in children do
		node.count += 1
		table.insert(log, parent .. ">" .. tostring(child))
	end
	if node.count > 3 then
		table.insert(log, "many")
	end
	return node
end

local function refresh(node, parent, children)
	update(node, parent, children)
end

local clock = { now = 0 }
function clock:IsRunning()
	self.now += 1
	return self.now % 2 == 1
end
local function running()
	return clock:IsRunning()
end

local function inc(n)
	return n + 1
end

local M = {}
function M.stop(self)
	disconnect(self.a)
	disconnect(self.b)
	disconnect(self.c)
end
function M.close(c)
	if c then
		c:Disconnect()
	end
end
function M.flush(self)
	if not isEmpty(self.queue) then
		return "pending"
	end
	return "empty"
end
function M.mix(x, y, k)
	local p = lerp(x, y, k)
	return math.clamp(lerp(y, x, k), 0, 1), p
end
function M.blend(x, y, k)
	local p = lerp(x, y, k)
	local q = lerp(y, x, k)
	return p, q, x + (y - x) * k
end
function M.solve(u, v, w)
	local d = det3(u[1], v[1], w[1], u[2], v[2], w[2], u[3], v[3], w[3])
	if d == 0 then
		return 0
	end
	return det3(w[1], v[1], u[1], w[2], v[2], u[2], w[3], v[3], u[3]) / d
end
function M.mount(node)
	refresh(node, "root", { 1, 2 })
	return node.count
end
function M.direct(node)
	refresh(node, "a", { 3 })
	update(node, "b", { 4 })
	return node.count
end
function M.tick()
	if running() then
		return "on"
	end
	return running() and "on" or "off"
end
function M.bump(n, m)
	return inc(n), m + 1
end

M.stop({ a = conn("a"), c = conn("c") })
M.close(conn("hand"))
print(table.concat(log, ","))
print(M.flush({ queue = {} }), M.flush({ queue = { 1 } }))
print(M.mix(1, 3, 0.25))
print(M.blend(2, 10, 0.5))
print(M.solve({ 1, 2, 3 }, { 0, 1, 4 }, { 5, 6, 0 }), M.solve({ 1, 1, 1 }, { 1, 1, 1 }, { 1, 1, 1 }))
local node = { count = 0 }
print(M.mount(node), M.direct(node), table.concat(log, ","))
print(M.tick(), M.tick(), M.tick())
print(M.bump(1, 2))
