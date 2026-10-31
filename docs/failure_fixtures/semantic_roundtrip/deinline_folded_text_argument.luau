-- Copies Luau -O2 made of `parseReward(label, text)` with constant labels:
-- `label .. " is not a number"` folded to `"Gold is not a number"` in the
-- copy. The text holds the helper's own text, and the label is what the
-- concatenation leaves of it: each is `parseReward("Gold", ...)` again.
local function parseReward(label, text)
	local trimmed = string.match(text, "^%s*(.-)%s*$")
	if trimmed == "" then
		return 0, nil
	end
	local number = tonumber(trimmed)
	if not number then
		return nil, label .. " is not a number"
	end
	local integer = math.floor(number)
	if integer < 0 then
		return nil, label .. " is negative"
	end
	return integer, nil
end

local function readRewards(goldText, gemText)
	local gold, goldError = parseReward("Gold", goldText)
	if goldError then
		return nil, goldError
	end
	local gem, gemError = parseReward("Gem", gemText)
	if gemError then
		return nil, gemError
	end
	return gold + gem
end

print(readRewards("5", "7"))
print(readRewards("x", "1"))
print(readRewards("2", "-3"))
