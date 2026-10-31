-- Copies Luau -O2 made of a helper whose select on a flag has call arms:
-- `setStatus(text)` leaves the flag `nil`, so the copy keeps only the
-- second arm (`label.Color = rgb(150, 255, 180)`); `setStatus(text, true)`
-- keeps `rgb(255, 120, 120) or rgb(150, 255, 180)`. Each is its call again.
function rgb(r, g, b)
	return r .. "," .. g .. "," .. b
end

local label = nil
local function setStatus(text, failed)
	if not label then
		return
	end
	label.Text = text
	label.Color = failed and rgb(255, 120, 120) or rgb(150, 255, 180)
end

function onClick(n)
	if n > 0 then
		setStatus("bad", true)
	else
		setStatus("ok")
	end
end

onClick(1)
label = {}
onClick(1)
print(label.Text, label.Color)
onClick(-2)
print(label.Text, label.Color)
