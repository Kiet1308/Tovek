-- An arithmetic helper choosing between two values (`helper(value, flag)`),
-- whose copy Luau -O2 leaves as `local result; if flag then result = ...
-- else result = ... end`, is rebuilt as `helper(value, flag)` by the
-- expression de-inliner's select form. With line info the helper also has
-- an evidence twin (plan E1) for sites saving less than the floor; the twin
-- waits for the evidence rounds and must never compete with the helper
-- itself in the ordinary walk, where two matches of one helper would refuse
-- the site as ambiguous (it did for a while: the copy stayed pasted, and
-- this fixture's output grew).
-- Negative: the second copy, `helper(value, false)` folded to its else
-- value, matches no select: it stays.
local function helper(value, flag)
	local result
	if flag then result = (value * 3 + 5) * 2 else result = (value * 7 + 4) * 3 end
	return result
end
local function run(value, flag, sink, tail)
	local first = helper(value, flag)
	local second = helper(value, false)
	return first, second
end
local M = { helper = helper, run = run }
print(M.run(1, true))
print(M.run(2, false))
print(M.helper(3, true), M.helper(4, false))
return M
