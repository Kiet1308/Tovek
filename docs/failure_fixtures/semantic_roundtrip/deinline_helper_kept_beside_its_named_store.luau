-- A helper whose binder only feeds the field of its own name (`M.clampDepth
-- = clampDepth`) once Luau -O2 inlined every call of it. The SSA inliner
-- used to fold that binder into `function M.clampDepth` before the
-- de-inliner looked, so the copies stayed pasted. Line info shows the
-- copies, so the binder now waits for the de-inliner, which rebuilds
-- `clampDepth(...)` and keeps `local function clampDepth` beside the store,
-- as the source has it; the same for a table field (`{ mode = mode }`).
-- Negatives: `shout`, inlined nowhere, still prints `function M.shout`;
-- `pick` is copied only as `table.clone(t)` for `pick(t)`, a copy no larger
-- than the call: no call is claimed for it, and its binder folds back into
-- `function M.pick` as before.
local modes = { fast = true, slow = true }
local M = {}
local function clampDepth(value)
	local n = tonumber(value)
	if not n then
		return 1
	end
	if n < 0 then
		return 0
	end
	return math.floor(n)
end
local function mode(name)
	if type(name) == "string" and modes[name] then
		return name
	end
	return "fast"
end
local function shout(s)
	return string.upper(s) .. "!"
end
local function pick(t, deep)
	if not deep then
		return table.clone(t)
	end
	local out = {}
	for k, v in t do
		out[k] = if type(v) == "table" then table.clone(v) else v
	end
	return out
end
M.clampDepth = clampDepth
M.shout = shout
M.pick = pick
function M.read(config)
	return { depth = clampDepth(config.depth), mode = mode(config.mode), limit = clampDepth(config.limit) }
end
function M.copy(t)
	local c = pick(t)
	c.copied = true
	return c
end
local api = { mode = mode, read = M.read }
local r = M.read({ depth = "7.5", mode = "slow", limit = -3 })
print(r.depth, r.mode, r.limit, api.mode(nil), api.mode("slow"))
print(M.clampDepth("x"), M.shout("hi"), M.copy({ 1, 2 }).copied, #M.pick({ 1, { 2 } }, true))
