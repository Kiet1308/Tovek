-- A repeating decimal the compiler folded from a fraction prints as that
-- fraction (`0.7272727272727273` as `8 / 11`), which compiles back to the
-- identical constant at every level: folded at -O1 and -O2, divided at run
-- time at -O0, and IEEE division is correctly rounded. The spelling groups
-- like a division (`x * (8 / 11)`, `2 ^ (1 / 3)`, `x - -8 / 11`), a literal
-- key keeps its decimal, and `task.wait(7 / 60)` is no longer hoisted into a
-- named constant. What has no short exact fraction stays a decimal: a widened
-- float, a denominator over 1000, one ulp off a fraction, a short decimal.
-- Each value prints with 17 significant digits, so a changed bit shows.
local function show(label, value)
	print(label, string.format("%.17g", value))
end

local scale = tonumber("3")

show("8/11", 0.7272727272727273)
show("11/30", 0.36666666666666664)
show("1/60", 0.016666666666666666)
show("69/56", 1.2321428571428572)
show("-13/60", -0.21666666666666667)
show("1/997", 0.0010030090270812437)
show("source 2/3", 2 / 3)
show("355/113", 3.1415929203539825)

-- Operands: the spelling takes the parentheses a division needs.
show("x*(8/11)", scale * 0.7272727272727273)
show("8/11*x", 0.7272727272727273 * scale)
show("x//(8/11)", 10 // 0.7272727272727273)
show("x%(8/11)", scale % 0.7272727272727273)
show("2^(1/3)", 2 ^ 0.3333333333333333)
show("(1/3)^x", 0.3333333333333333 ^ scale)
show("x - -8/11", scale - -0.7272727272727273)
show("-(1/3)", -(scale > 0 and 0.3333333333333333 or 0))
print("x<8/11", scale / 5 < 0.7272727272727273, scale / 4 < 0.7272727272727273)
print("concat", "r=" .. 0.36666666666666664)

-- A table value and a call argument are spelled; a literal key stays.
local ratios = { half = 0.5, third = 0.3333333333333333, [0.3333333333333333] = "third key" }
show("field", ratios.third)
print("key", ratios[1 / 3], ratios[0.3333333333333333])
show("call", math.max(0.11666666666666667, 0.1))

-- Three waits of one duration: a role `rehoist_constants` used to name.
local waited = {}
task = {
	wait = function(seconds)
		waited[#waited + 1] = seconds
	end,
}
task.wait(0.11666666666666667)
task.wait(0.11666666666666667)
task.wait(0.11666666666666667)
show("waits", waited[1] + waited[2] + waited[3])
print("same", waited[1] == 7 / 60, waited[2] == waited[3])

-- These stay decimals.
show("float 0.34", 0.3400000035762787)
show("1000/1001", 0.999000999000999)
show("ulp off 8/11", 0.7272727272727274)
show("short", 0.35 + 0.125)
show("integer", 5 + 0.5)
show("pi", 3.141592653589793)
print("zero", 1 / -0, 1 / 0)
