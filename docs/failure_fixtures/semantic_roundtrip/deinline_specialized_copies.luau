-- Copies Luau -O2 made for a constant argument it folded away, with another
-- shape than the helper's body. `copy(v, true)` keeps only what follows the
-- `if not deep then return table.clone(t) end`: the local recursive
-- `deepCopy`, a function literal Luau shares (DUPCLOSURE), and its call.
-- The literal is only ever called, so no code sees which object runs and
-- the copy is rebuilt as `copy(v, true)` (as a store and as a declaration).
-- `layout(false)` keeps two of the four statements of `layout`'s `if`.
-- Each specialization is matched only after every other helper is stable:
-- `paint(b, n >= 50)` is one copy holding both specializations in its
-- arms, rebuilt whole first and never as two calls.
-- Negatives: `copy(t)` alone is `table.clone(t)`, no larger than the call:
-- it stays; `share` returns its shared literal, whose identity is seen.
local log = {}
local function copy(t, deep)
	if not deep then
		return table.clone(t)
	end
	local function deepCopy(tbl)
		local out = table.clone(tbl)
		for k, v in out do
			if type(v) == "table" then
				out[k] = deepCopy(v)
			end
		end
		return out
	end
	return deepCopy(t)
end
local frame = { Size = 0, Position = 0 }
local function layout(portrait)
	if portrait then
		frame.Size = 10
		frame.Position = 60
	else
		frame.Size = 40
		frame.Position = 30
	end
end
local function paint(button, enabled)
	if enabled then
		button.Auto = true
		button.Alpha = 0
	else
		button.Auto = false
		button.Alpha = 0.5
	end
end
local function share()
	local function cb()
		return 1
	end
	return cb
end
local M = {}
function M.sync(src, template)
	local tbl = copy(src)
	for k, v in template do
		if src[k] == nil and type(v) == "table" then
			tbl[k] = copy(v, true)
		end
	end
	local deep = copy(template, true)
	return tbl, deep
end
function M.setup(n)
	layout(false)
	local b = {}
	paint(b, n >= 50)
	return frame.Size .. "/" .. frame.Position .. " " .. tostring(b.Auto) .. " " .. b.Alpha
end
local a, b = M.sync({ x = 1 }, { y = { z = { 2 } }, x = 5 })
print(a.x, a.y.z[1], a.y ~= b.y, b.y.z[1], M.setup(60), M.setup(10))
print(share() == share(), #log)
