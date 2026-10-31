-- A helper returning a default its caller left out, read through
-- `typeof(default) == "string"`, after branches that return early (`if
-- entries then local value = entries[key]; if ok then return value end
-- end; return if typeof(default) == "string" then default else ""`). The
-- statements after the branches run at each end they fall off, and Luau
-- folds `typeof(nil)` in the copies, which store `""` at both: each copy
-- is the call without the default.
local assets = { event = { login = { board = "rbxassetid://1", badge = "" } } }

local function pickAsset(group, key, default)
	local entries = assets.event[group]
	if typeof(entries) == "table" then
		local value = entries[key]
		if typeof(value) == "string" and value ~= "" then
			return value
		end
	end
	return if typeof(default) == "string" then default else ""
end

local theme = {
	Board = pickAsset("login", "board"),
	Badge = pickAsset("login", "badge"),
	Missing = pickAsset("shop", "icon"),
	Named = pickAsset("shop", "icon", "fallback"),
}
print(theme.Board, theme.Badge == "", theme.Missing == "", theme.Named)
