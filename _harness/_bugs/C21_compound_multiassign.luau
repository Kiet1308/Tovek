-- Compound assignment on indexed targets with side-effecting index expressions
-- (index evaluated once), scalar compounds, string ..= accumulation, parallel
-- swaps and multi-assignment truncation/expansion ordering.
local log = {}
local t = { a = 1, b = 2 }
local function idx(name)
	log[#log + 1] = name
	return name
end
t[idx("a")] += 10
t.b *= 3
t.b -= 5
t.b %= 4
t.b ^= 2
print(t.a, t.b, table.concat(log, ","))
local s = "x"
s ..= "y"
s ..= "z"
print(s)
local x, y = 1, 2
x, y = y, x
x, y = x + y, x - y
print(x, y)
local function g() return 1, 2, 3 end
local a1, b1, c1 = g(), 10, g()
print(a1, b1, c1)
local u, v = g()
print(u, v)
local pair = { x = 1, y = 2 }
pair.x, pair.y = pair.y, pair.x
print(pair.x, pair.y)
local t2 = {}
local function keyFn() log[#log + 1] = "k"; return 1 end
t2[1] = 1
t2[keyFn()] += 5
print(t2[1], table.concat(log, ","))