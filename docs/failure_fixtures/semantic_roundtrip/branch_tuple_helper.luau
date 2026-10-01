-- A helper returning two values through branches inlines as stores into the
-- caller's locals, the local it returns sharing its result's register:
-- `local i = find(...); local name; if i then name = sub(...) else name = s;
-- i = 1 end`. Rebuilt, `local name, at = split(s)` reads the same values.
-- A call before the last value is cut to one result either way. A copy that
-- stores a result in its argument's own register (`a = sub(a, ...)`) stays.
local function split(value: string)
	local at = string.find(value, "%s")
	if at then
		return string.sub(value, 1, at - 1), at
	end
	return value, 1
end

local function escape(text: string)
	local escaped = text:gsub("\n", "\\n")
	local double = escaped:find('"')
	local single = escaped:find("'")
	if double and single then
		return escaped:gsub('"', '\\"'), '"'
	end
	if single then
		return escaped, '"'
	end
	return escaped, "'"
end

local function order(list)
	table.sort(list, function(a, b)
		local nameA, atA = split(a)
		local nameB, atB = split(b)
		if nameA ~= nameB then
			return nameA < nameB
		end
		return atA < atB
	end)
	return list
end

local function quote(text)
	local escaped, mark = escape(text)
	return mark .. escaped .. mark
end

print(table.concat(order({ "beta two", "alpha", "beta", "alpha one" }), ","))
print(quote("it's"), quote('say "hi" it\'s'), quote("line\nnext"))
