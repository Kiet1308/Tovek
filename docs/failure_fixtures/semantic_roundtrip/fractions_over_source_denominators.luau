-- A fraction prints over the denominator its source most likely divided by,
-- not always its lowest one. The decimals below are what the compiler keeps
-- of `190 / 255`, `28 / 60`, ... (written as decimals here so they are
-- constants at every optimization level).
-- - Color channels: 0.7450980392156863 is 38/51 in lowest terms; 51 = 3 * 17
--   is no denominator anyone writes, so it prints `190 / 255`.
-- - Screen layout ratios over 480, 960 and 540 print over 1920 and 1080
--   once two such fractions agree on one (`1492 / 1920`, `814 / 1080`).
-- - Frames: beside four sixtieths, `7 / 15` and `2 / 3`, alone with their
--   denominators, print `28 / 60` and `40 / 60`; the sixtieths stay.
-- - `8 / 11` has no such denominator, and the unit fractions `1 / 7` and
--   `1 / 30` (rates and steps) never move: all stay in lowest terms.
-- Every spelling divides to the same double (printed with %.17g).
local values = {
	color = { 0.7450980392156863, 0.7843137254901961 },
	frames = { 0.18333333333333332, 1.1166666666666667, 0.21666666666666667, 0.48333333333333334 },
	alone = { 0.4666666666666667, 0.6666666666666666 },
	layout = { 0.7770833333333333, 0.6135416666666667, 0.7537037037037037, 0.2537037037037037 },
	stay = { 0.7272727272727273, 0.14285714285714285, 0.03333333333333333 },
}

local function show(name)
	local out = {}
	for _, value in values[name] do
		table.insert(out, string.format("%.17g", value))
	end
	print(name, table.concat(out, " "))
end

local function tween(duration)
	return duration * 60
end

for _, name in { "color", "frames", "alone", "layout", "stay" } do
	show(name)
end
print(tween(0.4666666666666667), tween(0.18333333333333332), 2 * 0.7450980392156863)
