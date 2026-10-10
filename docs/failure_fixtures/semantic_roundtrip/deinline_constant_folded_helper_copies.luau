-- Copies of pure helpers that Luau -O2 folded whole into one constant
-- (plan E2, the constant ledger). `frames(13)` inlines to the constant
-- 0.21666666666666667; line info keeps `frames`' line on its load, so the
-- lifter counts such loads per function and value. A literal becomes the
-- call where every reference to that constant in the function is such a
-- copy, the tree holds exactly that many literals of it, and an argument
-- folds back to it bit for bit: `wait(frames(13))`, `frames(143)`,
-- `scale(-3)` (two operations, a negative argument). The copies of
-- `frames` inside copies of `linear` become calls too, after which
-- `linear(13)` matches exactly. The function `later` returns is built in
-- `later` and again in its copy in the chunk: each instance holds its own
-- `frames(90)` copy, and each is rebuilt.
-- Negatives: `mixed` holds one `frames(6)` copy and a hand-written 0.1,
-- the same constant: both stay 0.1, while its `frames(21)` copy is
-- rebuilt. `square(3)` folds through `^`, `half(7)` through `math.floor`
-- (a builtin a script may replace): neither helper is ever rebuilt from a
-- constant. `byHand` writes 0.5 with no copy of anything: it stays.
-- Without line info (-g0) every constant stays as it was.
local log = {}
local function record(...)
	local parts = {}
	for i = 1, select("#", ...) do
		parts[i] = tostring((select(i, ...)))
	end
	table.insert(log, table.concat(parts, " "))
end

local function frames(n: number)
	return n / 60
end

local function scale(n: number)
	return -n * 2.5
end

local function square(x: number)
	return x ^ 2
end

local function half(x: number)
	return math.floor(x / 2)
end

local function run(tween, wait)
	local function linear(n: number)
		return { time = frames(n), style = "Linear" }
	end
	tween(linear(13), 25)
	wait(frames(13))
	tween(linear(21), 65)
	wait(frames(21))
	tween(linear(6), 30)
	wait(frames(6))
	wait(frames(143))
	record("scaled", scale(-3), scale(4))
end

local function mixed(wait)
	wait(frames(6))
	wait(0.1)
	wait(frames(21))
	record("folded", square(3), half(7))
end

local function byHand(wait)
	wait(0.5)
end

local function later()
	return function(k)
		return frames(k), frames(90)
	end
end

local function tween(info, fov)
	record("tween", info.time, info.style, fov)
end
local function wait(seconds)
	record("wait", seconds, 1 / seconds)
end

run(tween, wait)
mixed(wait)
byHand(wait)
record("later", later()(30))
for _, line in log do
	print(line)
end
