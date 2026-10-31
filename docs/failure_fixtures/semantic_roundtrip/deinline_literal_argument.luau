-- Copies Luau -O2 made of `activate(key, callback)` passed a function
-- literal: the copy holds the literal's code where it calls `callback`, and
-- the caller still makes the literal (NEWCLOSURE capturing `target`), which
-- nothing reads. The literal is kept aside, its code inside the copy is its
-- call, and that call is the callback again: `activate("Garden", function()
-- ... end)`, returned or assigned. `onShop`'s literal folded whole with the
-- `teleport` copy in it (`name ~= ""` is `true`), so its copy stays code.
local lastFire = {}

local function activate(key, callback)
	local now = os.clock()
	local last = lastFire[key]
	if last and now - last < 1 then
		return false
	end
	local ok = callback()
	if ok then
		lastFire[key] = now
	end
	return ok
end

local function teleport(name)
	print("teleport", name)
	return name ~= ""
end

function onShop()
	return activate("Shop", function()
		return teleport("Emilia")
	end)
end

function onGarden(target)
	return activate("Garden", function()
		print("garden", target)
		return teleport(target)
	end)
end

function onPlot(target)
	local ok = activate("Plot", function()
		print("plot", target)
		return teleport(target)
	end)
	print("plot done", ok)
	return ok
end

print(onShop(), onShop(), onGarden("plot"), onPlot("home"))
