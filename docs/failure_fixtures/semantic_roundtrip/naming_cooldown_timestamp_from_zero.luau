-- A cooldown timestamp that starts at 0 and is later set from os.clock() keeps a name for what it holds.
local Cooldown = {}

function Cooldown.new(limit)
	local lastUse = 0
	local uses = 0
	return function(message)
		if message == "/stop" then
			return "stopped"
		elseif not (os.clock() - lastUse < limit) then
			lastUse = os.clock()
			uses += 1
			return "used " .. uses
		end
		return "cooling"
	end
end

local tryUse = Cooldown.new(-1)
print(tryUse("go"), tryUse("go"), tryUse("/stop"))
local slow = Cooldown.new(1e9)
print(slow("go"))
