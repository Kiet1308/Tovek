-- V2.6 review fr-core F6: refused at -O2 -g1/-g2, a state machine at -g0.
-- At -O2 `f(1)` is inlined: `return g` inside the repeat becomes "store the result, jump
-- past `r = -1`", and the `until` exit lands in that `r = -1`, which the `if` skipping
-- the loop also reaches. The loop exit now runs its own copy: `r = -1 break`.
-- Expected output: 1, -1, -1.
local t = {false, true}
local g = 0
local function f(x)
	if not t[x] then
		repeat
			g += 1
			if t[x + 1] then
				return g
			end
		until g >= 5
	end
	return -1
end
print(f(1), f(2), f(3))
