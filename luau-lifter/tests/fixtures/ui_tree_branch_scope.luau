local Fusion = require(script.Parent.Fusion)
local Children = Fusion.Children
local OnEvent = Fusion.OnEvent

return function(parent, outer)
	-- Assigned on both paths, then captured: settled before its capture.
	local scope
	if outer then
		scope = Fusion.innerScope(outer, Fusion)
	else
		scope = Fusion.scoped(Fusion)
	end
	local function prompt()
		print("prompt", scope)
	end
	prompt()
	scope:New "Frame" {
		Parent = parent,
		[Children] = {
			scope:New "UIScale" {
				Scale = scope:DeviceScale(),
			},
			scope:PrimaryButton {
				Text = "Test",
				[OnEvent "Activated"] = prompt,
			},
		},
	}
end
