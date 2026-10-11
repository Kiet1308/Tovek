-- When the structurer finds no join for a conditional, it copies the code
-- both arms reach into each arm (here the `for` after the `repeat`, which it
-- emitted three times). The copies shared their locals, so the declaration
-- of `k`'s cell (written in the body, captured by a closure, fresh in every
-- iteration) was hoisted above all three copies: `local v2` before the
-- outer `if`, one cell for every iteration, and every closure returned the
-- last value: `1 2 / 2 2 / 3 2 / 4 2` for `1 1 / 2 2 / 3 1 / 4 2` (fr-core F4,
-- in V2.5.1 too). Each copy now gets cells of its own, and the copies'
-- closures, which shared one body, get a body each once linked.
local sink = {}
local function body(a, b, ...)
	local g = 0
	repeat
		g += 1
		while a <= 3 do
			if b == 99 then
				return -1
			end
			break
		end
		if a == a then
			a, b = b, a
			if 10 - a == math.max(3, a) then
				break
			end
		end
	until g >= 4 or a % 2 == 0
	for k in pairs({ 10, 20 }) do
		k = (select(2, ...) or k % 3)
		sink[#sink + 1] = function()
			return k
		end
	end
end
body(1, 2, 10, nil, 11)
body(5, -3, 10, nil, 11)
body(7, 7, 10, nil, 11)
for i, f in ipairs(sink) do
	print(i, f())
end
