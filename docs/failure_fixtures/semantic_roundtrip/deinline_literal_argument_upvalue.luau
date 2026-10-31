-- Copies Luau -O2 made of `activate(key, fn)` inside handler literals, each
-- passed a function literal: the copy holds the literal's code where it calls
-- `fn`, and the handler still makes the literal, capturing the handler's own
-- upvalue `teleportToNpc`. The literal's code inside the copy is part of it
-- (one copy of `activate` each), its capture is that upvalue as the handler
-- reads it, and rebuilding the call keeps every literal shared as Luau shares
-- it. Each handler is `activate("Shop", function() ... end)` again.
local lastFire = {}

local function teleportToNpc(name, ...)
	print("teleport", name, select("#", ...))
	return name ~= "nobody"
end

local function activate(key, fn)
	local now = os.clock()
	local last = lastFire[key]
	if last and now - last < 1 then
		return false
	end
	local ok = fn()
	if ok then
		lastFire[key] = now
	end
	return ok
end

local handlers = {
	onShop = function()
		return activate("Shop", function()
			return (teleportToNpc("Emilia"))
		end)
	end,
	onPot = function()
		return activate("Pot", function()
			return (teleportToNpc("Jack"))
		end)
	end,
}

for _, name in { "onShop", "onPot", "onShop" } do
	print(name, handlers[name]())
end
