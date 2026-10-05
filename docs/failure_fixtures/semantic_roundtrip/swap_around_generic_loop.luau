-- Two locals swapped in a loop that also holds a branch and a generic
-- `for`: the values the `for` carries need copies coalescing cannot
-- remove, and SSA destruction left them at the head of the `for` step's
-- block. No source runs code before a `for` step, so structuring failed
-- ("residual goto"). The copies now run at the end of every edge into the
-- step instead. Found by the typed fuzzer.
local count = 0
local function stop()
	count += 1
	return count < 4
end

local function swapped(input, flip, items)
	local a, b = input, if flip then 1 else -1
	while stop() do
		a, b = b, a
		if a < input then
			a -= 1
		end
		for _ in items do
			local inner = 0
			while inner < 1 do
				inner += 1
			end
		end
	end
	return a, b
end

print(swapped(5, true, { 1 }))
count = 0
print(swapped(5, false, {}))
