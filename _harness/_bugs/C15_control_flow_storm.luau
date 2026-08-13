-- Control-flow storm: 5-way elseif chain, and/or short-circuit eval order,
-- while + continue, repeat/until with continue AND break, nested conditionals.
local log = {}
local function mark(name) log[#log + 1] = name; return name end
local function classify(x)
	if x < 0 then return "neg"
	elseif x == 0 then return "zero"
	elseif x < 10 then return "small"
	elseif x > 10 and x < 100 then return "med"
	else return "big" end
end
print(classify(-5), classify(0), classify(7), classify(50), classify(500))
local function ok() log[#log + 1] = "ok"; return true end
local function no() log[#log + 1] = "no"; return false end
local r1 = no() and ok()
local r2 = ok() or no()
local r3 = (no() and ok()) or ok()
print(r1, r2, r3, table.concat(log, ","))
local acc = 0
local i = 0
while i < 10 do
	i += 1
	if i % 2 == 0 then continue end
	acc += i
end
print(acc)
local n, total = 0, 0
repeat
	n += 1
	if n == 3 then continue end
	if n >= 6 then break end
	total += n
until n >= 50
print(n, total)
local out = {}
for j = 1, 6 do
	for k = 1, 6 do
		if k == 4 then continue end
		if j > 3 then break end
		out[#out + 1] = j * 10 + k
	end
end
print(table.concat(out, ","))