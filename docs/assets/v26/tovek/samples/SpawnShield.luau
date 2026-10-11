-- SpawnShield: a new character gets a short glow while it is safe to fight.
local Players = game:GetService("Players")
local ReplicatedStorage = game:GetService("ReplicatedStorage")

local SHIELD_SECONDS = 3
local MAX_TINTED = 12
local COOLDOWN = 30
local GLOW_TEMPLATE = ReplicatedStorage:WaitForChild("ShieldGlow")

local function frames(count)
	return count / 60
end

local function isAlive(humanoid)
	return humanoid ~= nil and humanoid.Health > 0
end

local function shield(character)
	local humanoid = character:FindFirstChildOfClass("Humanoid")
	if not isAlive(humanoid) then
		return
	end

	local glow = GLOW_TEMPLATE:Clone()
	glow.Transparency = 2 / 3
	glow.Parent = character

	local tinted = 0
	for _, part in character:GetDescendants() do
		if part:IsA("BasePart") then
			if part.Transparency == 1 then
				continue
			end
			part.Color = glow.Color
			tinted += 1
		end
		if tinted >= MAX_TINTED then
			break
		end
	end

	local started = os.clock()
	while os.clock() - started < SHIELD_SECONDS do
		if not isAlive(humanoid) then
			break
		end
		task.wait(frames(1))
	end
	glow:Destroy()
end

local function onPlayerAdded(player)
	local character = player.Character or player.CharacterAdded:Wait()
	if character:IsDescendantOf(workspace) then
		shield(character)
	end

	local lastUse = 0
	local connection
	if player.MembershipType == Enum.MembershipType.Premium then
		connection = player.Chatted:Connect(function(message)
			if message == "/stop" then
				connection:Disconnect()
			elseif not (os.clock() - lastUse < COOLDOWN) then
				lastUse = os.clock()
				shield(player.Character)
			end
		end)
	end
end

Players.PlayerAdded:Connect(onPlayerAdded)
