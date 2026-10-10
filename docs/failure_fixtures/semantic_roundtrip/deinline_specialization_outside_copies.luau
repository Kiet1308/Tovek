-- A copy specialized for a constant argument (Tier B) claims the helper ran
-- with a constant its copy folded away. Line info shows where Luau -O2
-- inlined the helper (plan E1, honesty part b): `close` runs a real copy of
-- `show(nil)`, rebuilt as that call, as before.
-- Negative: `watch` runs the same statements written by hand, in a function
-- where line info shows no code of `show`: they stay as written, though
-- they equal `show(nil)` (they were rebuilt as that call before).
-- Negative: `reset` writes `clear(t, false)`'s statements by hand where no
-- code of `clear` is: they stay too.
-- Without line info (-g0) both negatives are rebuilt as before.
local log = {}
local state = { speaker = "npc", node = 1 }
local function hide()
	table.insert(log, "hide")
end

local function show(data)
	if data then
		state.speaker = data.speaker
		state.node = data.node
		table.insert(log, "show " .. tostring(data.node))
		return
	end
	hide()
	state.speaker = nil
	state.node = nil
end

local function clear(t, deep)
	if deep then
		for k in t do
			t[k] = nil
		end
		return
	end
	t.Anchored = true
	t.CanCollide = false
	t.CastShadow = false
end

local M = {}
function M.open(npc)
	show({ speaker = npc, node = 7 })
end
function M.close()
	show(nil)
end
function M.watch(far)
	if far then
		hide()
		state.speaker = nil
		state.node = nil
	end
	return state.speaker
end
function M.wipe(t)
	clear(t, true)
	return next(t)
end
function M.flat(t)
	clear(t, false)
	return t.Anchored
end
function M.reset(t)
	t.Anchored = true
	t.CanCollide = false
	t.CastShadow = false
	return t.CanCollide
end

M.open("guide")
print(state.speaker, state.node)
M.close()
print(state.speaker, state.node)
M.open("again")
print(M.watch(false), M.watch(true), table.concat(log, ","))
print(M.wipe({ 1, 2 }), M.flat({}), M.reset({}))
