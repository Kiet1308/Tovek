-- A nested loop followed directly by `break`. Luau threads the jump of the
-- `break`, so every exit of the inner loop targets the outer loop's exit.
-- (Reported: `for` + `repeat ... until {}` + `break` failed to structure.)
local log = {}
for i = 1, 3 do
	repeat
		log[#log + 1] = "r" .. i
	until true
	break
end
for i = 1, 3 do
	repeat
		if i == 1 then
			break
		end
		log[#log + 1] = "skip"
	until true
	break
end
for _, v in ipairs({ 10, 20 }) do
	repeat
		log[#log + 1] = "g" .. v
	until {}
	break
end
local n = 0
while n < 10 do
	n += 1
	for j = 1, 5 do
		if j == 2 then
			break
		end
		log[#log + 1] = "w" .. n .. j
	end
	break
end
print(table.concat(log, ","))
