-- Copies Luau -O2 made of `round(value, places)` with constant `places`:
-- the compiler folded `math.pow(10, places)` into 100 or 10000, so the
-- helper's first statement is gone from every copy. The number the copy
-- shows is solved back to `places` (`10^places = 10000` is 4), the copy is
-- shorter than the helper, and its result is returned, stored into the
-- caller's own local, or computed from an argument expression (`value /
-- 100`) that ran before the body. The helper keeps a plain `math.pow`:
-- a de-inlined call's copy was already folded.
local function round(value, places)
	local factor = math.pow(10, places)
	return math.round(value * factor) / factor
end

local function present(kind, value)
	if kind == "Float" then
		print("float")
		value = round(value, 2)
	elseif kind == "Percent" then
		print("percent")
		value = round(value / 100, 4)
	end
	return value
end

local function scaled(kind, value)
	if kind == "Basis" then
		value = round(value, 4)
	end
	return value
end

local function exact(value)
	return round(value, 3)
end

local fns = { present, scaled, exact, round }
print(fns[1]("Float", 1.23456), fns[1]("Percent", 12.345678), fns[1]("Int", 3))
print(fns[2]("Basis", 0.123456789), fns[2]("Other", 0.5), fns[3](2.71828), fns[4](9.87654, 1))
