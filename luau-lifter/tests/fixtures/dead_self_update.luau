local function mount(attachment, instance)
	local count = 0
	if attachment:IsA("Attachment") then
		attachment:Clone().Parent = instance
		count += 1
	else
		for _, child in attachment:GetChildren() do
			child:Clone().Parent = instance
			count += 1
		end
		if count == 0 then
			instance.Name = "empty"
			count += 1
		end
	end
	print(instance)
end

return mount
