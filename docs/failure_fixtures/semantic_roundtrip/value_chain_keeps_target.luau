-- `return ok and result or default`: Luau keeps `default` in the result
-- register and the jumps read as `if ok then default = result or default
-- end; return default`, a written parameter. That is `ok and result or
-- default` itself; the helper returns it again, so its copies passing a
-- default, or `nil` (`flipbookMode = ok and result or nil`), are calls.
local Modes = { Loop = "loop", OneShot = "oneshot" }

local function safeEnum(enumType, value, default)
	if not value then
		return default
	end
	local ok, result = pcall(function()
		return enumType[value]
	end)
	return ok and result or default
end

local function read(config)
	local data = {}
	data.Mode = safeEnum(Modes, config.Mode, Modes.Loop)
	data.Next = safeEnum(Modes, config.Next, Modes.OneShot)
	data.Flipbook = {
		Mode = safeEnum(Modes, config.FlipbookMode, nil),
		Framerate = config.Framerate or 30,
	}
	return data
end

for _, config in { { Mode = "OneShot", FlipbookMode = "Loop" }, { Next = "Loop" }, {} } do
	local data = read(config)
	print(data.Mode, data.Next, data.Flipbook.Mode, data.Flipbook.Framerate)
end
