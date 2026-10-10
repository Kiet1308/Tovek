-- A literal made in a loop that captures only a local function the loop
-- body declares (`h`, whose own literal captures nothing): Luau shares it
-- (DUPCLOSURE) from -O1 on, because a local initialized with a shareable
-- function literal keeps a literal capturing it shareable wherever it is
-- declared. The output printed a per-trip snapshot of `h` before the
-- literal (`local h2 = h`): the snapshot pass took the body's `local
-- function h`, which binds `h` anew on every trip, for a write of the loop.
-- The snapshot is no function literal, so the recompiled literal was new on
-- every trip and `seen[1] == seen[2]` printed false (at -g1 and -g2, where
-- the snapshot is named after `h` and no cleanup folds it back). A `local`
-- statement in the body is no write of the loop: no snapshot.
-- `label` is a body local captured by value too; each trip keeps its own.
-- Negative: `tick` is assigned again after its literal in the same trip;
-- the literal captures it by reference and reads the later value.
local seen, again = {}, {}
for i = 1, 2 do
	local function h()
		return 7
	end
	seen[i] = function()
		return h
	end
end
print(seen[1] == seen[2], seen[2]()())

local names = {}
for i = 1, 3 do
	local label = "item" .. i
	names[i] = function()
		return label
	end
end
print(names[1](), names[3](), names[1] == names[2])

for i = 1, 2 do
	local tick = i * 10
	again[i] = function()
		return tick
	end
	tick = tick + 1
end
print(again[1](), again[2]())
