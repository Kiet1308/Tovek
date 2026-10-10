-- Copies whose constant arguments Luau -O2 folded into their code (plan E2,
-- constant-fold equations). `fade(a, 0.5)` inlines `1 - k` as 0.5 and
-- `k * 10` as 5: where a value the helper computes from parameters meets a
-- number of the site, the match records an equation, solves the argument
-- and proves every equation by Luau's folding rules, bit for bit. A number
-- an equation stands for must be one the helper's copies in that function
-- refer to (line info), and the helper is rebuilt in a function only where
-- its matches with the calls already rebuilt there are exactly its copies
-- there: `fade(a, 0.5)`, `fade(b, 0.25)`, `map(y, 100, 350, 0.28, 1)`
-- (two parameters solved from two equations), `pick(0.25)` and
-- `pick(0.5)` (a pure helper called inside the helper, `half(n)`),
-- `linear(6)` beside a hand-written 0.1 (the constant ledger refuses 0.1
-- there, the equation does not need it).
-- Negatives: `byHand` builds `{ time = 0.5, style = "Linear" }` by hand
-- beside a `linear(6)` copy: 0.5 is no constant of a copy there, it stays.
-- `twice` holds one `fade` copy and the same statements by hand: two
-- matches for one copy, both stay. `square(c, 3)` folds through `^` and
-- `floored(c, 7)` through `math.floor` (a builtin a script may replace):
-- neither is rebuilt from its constant.
-- Without line info (-g0) every copy stays as it was.
local log = {}
local function record(...)
	local parts = {}
	for i = 1, select("#", ...) do
		parts[i] = tostring((select(i, ...)))
	end
	table.insert(log, table.concat(parts, " "))
end

local function part(name)
	local self = { Name = name, Transparency = 0 }
	function self:SetAttribute(key, value)
		record(self.Name, key, value)
	end
	return self
end

local function fade(p, k)
	p.Transparency = 1 - k
	p.Name = "Faded"
	p:SetAttribute("Level", k * 10)
end

local function map(x, inMin, inMax, outMin, outMax)
	return (x - inMin) * (outMax - outMin) / (inMax - inMin) + outMin
end

local function frames(n: number)
	return n / 60
end

local function half(n: number)
	return n / 2
end

local function pick(t: number)
	return { at = half(t) + 1, tag = "pick" }
end

local function square(p, k)
	p.Size = k ^ 2
	p.Name = "Squared"
end

local function floored(p, k)
	p.Size = math.floor(k / 2)
	p.Name = "Floored"
end

local function main(y, z)
	local a, b, c = part("a"), part("b"), part("c")
	fade(a, 0.5)
	fade(b, 0.25)
	fade(c, z)
	record(a.Transparency, b.Transparency, c.Transparency)
	record(map(y, 100, 350, 0.28, 1), map(y, 50, 100, 0, 1))
	local p1, p2 = pick(0.25), pick(0.5)
	record(p1.at, p1.tag, p2.at)
	square(c, 3)
	floored(a, 7)
	record(c.Size, a.Size, c.Name, a.Name)
end

local function run(tween, wait)
	local function linear(n: number)
		return { time = frames(n), style = "Linear" }
	end
	tween(linear(6), 1)
	wait(0.1)
	tween(linear(12), 2)
end

local function byHand(tween)
	local function linear(n: number)
		return { time = frames(n), style = "Linear" }
	end
	tween(linear(6), 1)
	tween({ time = 0.5, style = "Linear" }, 2)
end

local function twice(q, r)
	fade(q, 0.25)
	r.Transparency = 0.75
	r.Name = "Faded"
	r:SetAttribute("Level", 2.5)
end

main(170, 0.125)
local function tween(info, x)
	record("tween", info.time, info.style, x)
end
run(tween, function(s)
	record("wait", s)
end)
byHand(tween)
twice(part("q"), part("r"))
for _, line in log do
	print(line)
end
