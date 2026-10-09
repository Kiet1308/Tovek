-- A format call kept to one result (`local message = ("...%*"):format(p);
-- warn(message)` folded into the call) printed as `warn((`...{p}`))`: the
-- truncation parentheses wrapped a backtick string, which is one value
-- already. They are dropped in a call's last argument, a constructor's last
-- element and `return`'s last value whenever the call prints as a backtick
-- string; a `%s` format, which stays `:format`, keeps them, as does a format
-- whose last argument is an open call. `table.insert` counts its arguments,
-- so a second value would change what it does.
--
-- A format call the source leaves unadjusted in such a spreading position
-- (`{ ("item %*"):format(p) }`, `print(("c%*"):format(p))`, `return
-- ("x%*"):format(p)`) or that fills several targets (`local a, b =
-- ("%*"):format(p)`) keeps its `:format` form. Libraries may be hooked: a
-- replaced `string.format` may return several values there, and a backtick
-- string is exactly one (the bytecode shows it: SETLIST and CALL keep their
-- open result counts). A local helper that takes one parameter drops the
-- rest, so `emit(("id %*"):format(p))` still prints `` emit(`id {p}`) ``.
local log = {}

local function many()
	return "a", "b"
end

local function emit(text)
	table.insert(log, text)
end

local function describe(p)
	local message = ("Unable to find sell value for %*"):format(p)
	table.insert(log, message)
	local measured = ("%*!"):format(p)
	print(string.len(measured))
	local list = { ("item %*"):format(p) }
	table.insert(log, list[1])
	local plain = ("value %s"):format(p)
	table.insert(log, plain)
	local open = ("%* and %*"):format(many())
	table.insert(log, open)
	print(("c%*"):format(p))
	local first, second = ("%* pair"):format(p)
	table.insert(log, tostring(first) .. "/" .. tostring(second))
	emit(("id %*"):format(p))
	local secret = ("Secret %*"):format(p)
	return secret
end

local function direct(p)
	return ("x%*"):format(p)
end

print(select("#", describe("gem")), describe(42))
print(select("#", direct(1)), direct("y"))
print(#log, table.concat(log, "; "))
