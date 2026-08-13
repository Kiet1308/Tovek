-- O2 inlining bait: tiny single-expression helpers (LPF_INLINABLE candidates)
-- hammered in a hot loop; O0 keeps them as closures, O2 inlines them into the
-- loop body. The string builder forces de-inlining of interpolated accumulators.
local function sq(x) return x * x end
local function lerp2(a, b, t) return a + (b - a) * t end
local function clamp1(v, lo, hi) return math.max(lo, math.min(hi, v)) end
local function add3(a, b, c) return a + b + c end
local function id(x) return x end
local acc = 0
for i = 1, 1000 do
	acc += sq(i) + lerp2(0, 10, 0.5) + clamp1(i, 3, 7) + add3(i, 1, 2) + id(i)
end
print(acc)
local function tag(n) return `#<{n}>` end
local s = ""
for i = 1, 50 do
	s ..= tag(i)
end
print(#s, s:sub(1, 20))
local function foldDir(seed)
	return function(x)
		return x < seed and seed - x or x - seed
	end
end
local fn = foldDir(10)
local tot = 0
for i = 1, 100 do
	tot += fn(i) + sq(i % 7)
end
print(tot)