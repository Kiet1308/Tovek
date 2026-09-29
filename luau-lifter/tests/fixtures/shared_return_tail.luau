local function firstPersonOffset(character, cframe, thirdPerson)
	if character and character.Parent then
		local head = character:FindFirstChild("Head")
		if head and head:IsA("BasePart") then
			return cframe:Inverse() * head.Position
		end
	end
	return thirdPerson()
end

local function nameOf(x, defaultName)
	if typeof(x) == "table" then
		if typeof(x.name) == "string" then
			return x.name
		elseif typeof(x.kind) == "string" then
			return x.kind
		end
	end
	return defaultName
end

return { firstPersonOffset = firstPersonOffset, nameOf = nameOf }
