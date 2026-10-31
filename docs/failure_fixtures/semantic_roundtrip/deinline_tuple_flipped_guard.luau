-- A helper returning two values through guards on a truth (`if not number
-- then return nil, "not a number" end`): its copies keep the arm that
-- goes on first (`if number then ... else err = "not a number" end`). The
-- guard read either way round, each is `local count, err = parse(text)`.
local function parse(text)
	local trimmed = string.match(text, "^%s*(.-)%s*$")
	if trimmed == "" then
		return nil, nil
	end
	local number = tonumber(trimmed)
	if not number then
		return nil, "not a number"
	end
	local integer = math.floor(number)
	if integer <= 0 then
		return nil, "must be positive"
	end
	return integer, nil
end

local function readCount(text)
	local count, err = parse(text)
	if err then
		return "Count: " .. err
	end
	return count or 1
end

local function readLevel(text)
	local level, err = parse(text)
	if err then
		return "Level: " .. err
	end
	return level or 0
end

for _, text in { " 5 ", "", "x", "-2" } do
	print(readCount(text), readLevel(text))
end
