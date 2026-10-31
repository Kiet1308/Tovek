-- Copies Luau -O2 made of a helper returning two values through branches
-- (`return nil, nil`, `return nil, "not a number"`, `return integer, nil`),
-- stored into the caller's `count, err`. The copy declares both results in
-- one statement in its own order (`local err, count`), stores a leaf's
-- constant results in its own order, and keeps `integer`, declared in a
-- branch and returned as the first value alone, in the first result:
-- `count = math.floor(number)`. Each is `local count, err =
-- parseOptionalPositiveInteger(text)` again.
local function trimString(value)
	return (string.gsub(value or "", "^%s*(.-)%s*$", "%1"))
end

local function parseOptionalPositiveInteger(value)
	local text = trimString(value)
	if text == "" then
		return nil, nil
	end
	local number = tonumber(text)
	if number == nil then
		return nil, "not a number"
	end
	local integer = math.floor(number)
	if integer <= 0 then
		return nil, "must be positive"
	end
	return integer, nil
end

local function readCount(text)
	local count, err = parseOptionalPositiveInteger(text)
	if err then
		return nil, "Count: " .. err
	end
	return count or 1
end

local function readLength(text)
	local length, err = parseOptionalPositiveInteger(text)
	if err then
		return nil, "Length: " .. err
	end
	return math.clamp(length or 8, 4, 32)
end

for _, text in { " 5 ", "", "x", "-2", "40" } do
	print(readCount(text), readLength(text))
end
