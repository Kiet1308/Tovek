-- A rebuilt call that leaves out a trailing argument the helper reads.
-- `show(g(), nil)` inlines with `flag` folded away; the statement
-- de-inliner infers `nil` for it and leaves it out, but the argument
-- before it is a call: `show(g())` passes g's second result as `flag`
-- (here 2, printed as "flag 2" three times by the old output). Where the
-- last argument may give several values (a call, `...`), every parameter
-- the body reads keeps an argument: `show(g(), nil)`, `show(..., nil)`.
-- `count(g())` leaves out only a parameter the body never reads, which may
-- take the extra results: no `nil` is added there.
local log = {}
local function record(...)
	local parts = {}
	for i = 1, select("#", ...) do
		parts[i] = tostring((select(i, ...)))
	end
	table.insert(log, table.concat(parts, " "))
end

local function g(...)
	return 1, 2
end

local function show(item, flag)
	record("item", item)
	record("more", item)
	if flag then
		record("flag", flag)
	end
end

local function count(item, unused)
	record("count", item)
	record("again", item)
end

local function run(...)
	show(g(), nil)
	show(g(), false)
	show(g(), 5)
	show(..., nil)
	count(g())
end

run(7, 8)
for _, line in log do
	print(line)
end
