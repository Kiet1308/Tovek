-- A format call kept to one result (`local message = ("...%*"):format(p);
-- warn(message)` folded into the call) printed as `warn((`...{p}`))`: the
-- truncation parentheses wrapped a backtick string, which is one value
-- already. They are dropped in a call's last argument, a constructor's last
-- element and `return`'s last value whenever the call prints as a backtick
-- string; a `%s` format, which stays `:format`, keeps them, as does a format
-- whose last argument is an open call. `table.insert` counts its arguments,
-- so a second value would change what it does.
local log = {}

local function many()
	return "a", "b"
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
	local secret = ("Secret %*"):format(p)
	return secret
end

print(select("#", describe("gem")), describe(42))
print(#log, table.concat(log, "; "))
