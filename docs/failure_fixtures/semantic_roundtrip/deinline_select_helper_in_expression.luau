-- A copy Luau -O2 made of a helper choosing its value with `if c then
-- return a end return b`, evaluated inside an expression: the copy is the
-- select `not c and b or a`, which the helper's call stands for again.
local enabled = false
local part = {
	GetScale = function(self)
		return self.scale
	end,
	scale = 2,
}

local function getScale()
	if enabled then
		return part:GetScale()
	end
	return 1
end

function onRunning(speed)
	local scaled = speed / getScale()
	if scaled > 4 then
		print("running", scaled)
	else
		print("walking", scaled)
	end
end

onRunning(10)
enabled = true
onRunning(10)
part.scale = 5
onRunning(10)
