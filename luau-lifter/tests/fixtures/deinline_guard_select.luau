local function setup(signal)
	local function computeThrottle(dist)
		if dist > 0.2 then
			return 0.5 + dist ^ 2 / 2
		end
		return 0
	end

	local function formatBool(value)
		if value == nil then
			return "-"
		end
		if value == "Original" then
			return "original"
		end
		if value then
			return "on"
		end
		return "off"
	end

	local function charToHead(char)
		if not char or char.Parent == nil then
			return nil
		end
		local head = char:FindFirstChild("UpperTorso") or char:FindFirstChild("Torso")
		if head then
			return head
		end
		return nil
	end

	local function steer(target, origin)
		local offset = target - origin
		return computeThrottle(math.min(1, offset.Magnitude / 50)) * math.sign(offset.Z)
	end

	local function describe(settings, out)
		out[#out + 1] = "particles=" .. formatBool(settings.particleEnabled)
		out[#out + 1] = "beams=" .. formatBool(settings.beamEnabled)
		return out
	end

	local function track(char)
		local head = charToHead(char)
		if not head then
			return nil
		end
		return head.Position
	end

	for _, callback in { computeThrottle, formatBool, charToHead } do
		signal:Connect(callback)
		signal:Connect(callback)
	end
	return steer, describe, track
end

return setup
