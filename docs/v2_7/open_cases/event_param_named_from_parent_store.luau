-- Readability edge case (found while writing the landing sample, V2.6 tip e938654, -O2 -g1).
-- The CharacterAdded callback's argument is printed as `parent`: the `.Parent = x` store rule wins over the
-- event's own evidence (CharacterAdded passes a character). With a second store (`highlight.Adornee = character`)
-- the two store rules cancel and the event rule names it `character`.
local Players = game:GetService("Players")
Players.PlayerAdded:Connect(function(player)
	player.CharacterAdded:Connect(function(character)
		local humanoid = character:WaitForChild("Humanoid")
		local highlight = Instance.new("Highlight")
		highlight.FillColor = player.TeamColor.Color
		highlight.Parent = character
		print(humanoid.Health)
	end)
end)
