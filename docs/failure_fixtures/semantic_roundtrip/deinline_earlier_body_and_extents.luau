-- Two helpers whose copies meet in one statement.
-- Earlier body (dual-version patterns): `vary` holds an inlined copy of
-- `clamp01`, which the first round rebuilds as a call in `vary`'s body, so
-- `vary`'s pattern changes while its copies in `similar` still hold the
-- code. Its body as it was before that rewrite stays a pattern beside the
-- current one: `table.pack(vary(color.r), vary(color.g), vary(color.b))`,
-- the last one too, where only `clamp01` matched before.
-- Extents: where two helpers match values of one statement, the one whose
-- body stands for more code (its arguments not counted) wins, instead of
-- both being refused as ambiguous: `vary` over `clamp01` on one value, and
-- `print(area(w, h), ratio(w, h))` (before, `area`'s copy stayed).
-- Negative: `close` only calls `dropIcon`, so once that copy is a call it
-- is no helper, and its earlier body, `dropIcon`'s, is not kept: the copy
-- in `reopen` rebuilds as `dropIcon()`, not refused as two helpers alike.
local rng = { seed = 3 }
function rng:next(a, b)
	self.seed = (self.seed * 37 + 11) % 101
	return a + (b - a) * self.seed / 101
end
local function clamp01(value)
	return math.clamp(value, 0, 1)
end
local function similar(color, spread)
	local s = spread or 0.1
	local function vary(c)
		return clamp01(c + rng:next(-s, s))
	end
	return table.pack(vary(color.r), vary(color.g), vary(color.b))
end
local function area(w, h)
	return math.floor(w * h * 0.5 + math.sqrt(w + h))
end
local function ratio(w, h)
	return math.max(w, h) / math.min(w, h)
end
local function describe(w, h)
	print(area(w, h), ratio(w, h))
end
local state = { icon = nil, log = {} }
local function dropIcon()
	if state.icon then
		local icon = state.icon
		state.icon = nil
		table.insert(state.log, icon.name)
	end
end
local function close()
	dropIcon()
end
local function reopen(name)
	close()
	state.icon = { name = name }
end
-- Called through globals, so that Luau inlines neither here.
api = { similar = similar, describe = describe }
local p = api.similar({ r = 0.2, g = 0.5, b = 0.9 })
print(p.n, p[1], p[2], p[3])
p = api.similar({ r = 1.4, g = -0.3, b = 0.5 }, 0.5)
print(p.n, p[1], p[2], p[3])
api.describe(3, 4)
api.describe(5, 2)
state.icon = { name = "a" }
reopen("b")
reopen("c")
dropIcon()
print(table.concat(state.log, ","))
