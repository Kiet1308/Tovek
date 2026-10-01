local Nodes = require(script.Parent.Nodes)
local equipped = nil

-- Falls off the end when nothing is equipped.
local function getEquippedAsset()
	if equipped then
		return Nodes.GET_DATA_VALUE:InvokeSelf({ "FishingRodData", equipped, "Asset" })
	end
end

local function verify(instance)
	if instance and instance.Parent then
		return true
	end
	warn(`Unable to find object {instance}`)
end

-- Both arms return method calls.
local function getAsset(id)
	if id then
		return Nodes.Remote:InvokeSelf(id, "Asset")
	end
	return Nodes.Remote:InvokeSelf("default", "Asset")
end

return function(id, instance)
	equipped = id
	local equippedAsset = getEquippedAsset()
	if not verify(instance) then
		return
	end
	local asset = getAsset(id)
	print(equippedAsset and equippedAsset.Name, asset)
end
