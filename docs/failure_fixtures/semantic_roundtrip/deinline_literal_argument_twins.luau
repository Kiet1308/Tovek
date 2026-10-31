-- `activate(key, fn)` copies inside handler literals that Luau -O2 inlined
-- into the chunk as well: each chunk copy of a handler holds a copy of
-- `activate` holding the literal's code, and makes its own instance of the
-- literal (twins: one prototype, the same captures). The literal's code is
-- inside the nested copy, which goes on after it; each function's literals
-- are its own targets alone, twins one target. Every copy is `activate("Shop",
-- ...)` again; the chunk's twins, one shared constant passed on, are one
-- binding (`closure_identity`).
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

print(handlers.onShop(), handlers.onPot(), handlers.onShop())
