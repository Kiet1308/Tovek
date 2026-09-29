-- Builtin calls compile to FASTCALL: the arguments are evaluated first and
-- the callee (`table.insert`, `tostring`, `math.max`) is imported after them.
-- Folding the arguments back into the call must keep their order.
local log = {}
local function tick(value)
	log[#log + 1] = tostring(value)
	return value
end
local items = {}
table.insert(items, tostring(tick(1)))
table.insert(items, math.max(tick(2), tick(3)))
table.insert(items, string.format("%d-%d", tick(4), tick(5)))
table.insert(items, ("-"):rep(tick(2) + 1))
print(table.concat(items, ","), table.concat(log, ","))
