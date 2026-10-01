local RunService = game:GetService("RunService")
local Net = {}
function Net:RemoteEvent(name: string)
	return script:WaitForChild(name)
end
function Net:Connect(name: string, handler)
	if RunService:IsServer() then
		return self:RemoteEvent(name).OnServerEvent:Connect(handler)
	else
		return self:RemoteEvent(name).OnClientEvent:Connect(handler)
	end
end
function Net:Clean()
	script:ClearAllChildren()
end
return Net
