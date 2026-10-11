-- A loop whose header `if` keeps both arms inside the cycle (a `continue` in
-- a `repeat`, or a plain `while true`) is read as `while true`. Its body was
-- every block the header dominates, so it took in the block after the loop
-- as well when only the loop's own exits (a `break` and the `until` exit)
-- enter it: that block then sat in the body twice and structuring refused
-- with "residual goto/label would be invalid Luau" at -O0..-O2, in V2.5.1 too
-- (the repeat_continue_break edge case; fr-core F5, fr-dynamic F3, the same
-- with a nested `break` and no `continue`). The body now stops at the join of
-- the cycle's own exits, the loop's exit port. An inner cycle no longer takes
-- in the blocks of the enclosing one after it (nested `repeat` loops with a
-- `return`, fr-core F6b/F6c). A `break` arm of a conditional whose arms
-- meet inside the loop leaves through the exit port too (`power`: -O2 makes
-- its `while true` header the `%` test).
local function continueThenBreak()
	local seen = {}
	local i = 0
	repeat
		i += 1
		if i % 2 == 0 then
			continue
		end
		if i > 5 then
			break
		end
		seen[#seen + 1] = i
	until i > 10
	return table.concat(seen, ",")
end
print(continueThenBreak())

local function waitForHead(character)
	local i = 0
	repeat
		i += 1
		if character.Head == nil then
			continue
		end
		if i > 5 then
			break
		end
		character.ticks += 1
	until character.Parent == nil or i > 8
	return character
end
print(waitForHead({ Head = true, Parent = true, ticks = 0 }).ticks)
print(waitForHead({ Parent = true, ticks = 0 }).ticks)

local function twoBreaks()
	local out = {}
	local i = 0
	while true do
		i += 1
		if i % 2 == 0 then
			if i > 6 then
				break
			end
			out[#out + 1] = "even" .. i
		end
		if i > 9 then
			break
		end
	end
	return table.concat(out, ","), i
end
print(twoBreaks())

local function nestedBreakTail(limit)
	local out = {}
	local i = 0
	repeat
		i += 1
		if i % 2 == 1 then
			if i > limit then
				break
			end
			out[#out + 1] = i
		end
	until i > 10
	return table.concat(out, ","), i
end
print(nestedBreakTail(5))
print(nestedBreakTail(20))

local function nestedReturn(x)
	local a, b = 0, 0
	repeat
		a += 1
		repeat
			b += 1
			if b == x then
				return a * 10 + b
			end
		until b >= a
	until a >= 3
	return -1
end
print(nestedReturn(2), nestedReturn(4), nestedReturn(9))

local function nestedReturnAfterBreak(x)
	local a, b = 0, 0
	repeat
		a += 1
		if a > x then
			break
		end
		repeat
			b += 1
			if b == x then
				return a * 10 + b
			end
		until b >= a
	until a >= 3
	return -1
end
print(nestedReturnAfterBreak(0), nestedReturnAfterBreak(2), nestedReturnAfterBreak(4))

local function nestedRepeats(k)
	local out = {}
	local i, j = 0, 0
	repeat
		j += 1
		if k > 5 then
			repeat
				i += 1
			until i > 8 or k < 4
			repeat
				if i % 2 == 0 then
					out[#out + 1] = i
				end
				i += 1
			until i > 12 or i == 4
			if j > 30 then
				break
			end
		end
	until j % 3 == 0
	return table.concat(out, ","), i, j
end
print(nestedRepeats(6))
print(nestedRepeats(1))

local function power(n, base, limit)
	local result = 1
	while true do
		if n % 2 == 0 then
			n /= 2
		else
			n /= 2
			result = base * result
			if n < limit then
				return result
			end
		end
		base *= base
	end
end
print(power(5, 3, 1), power(12, 2, 1))
