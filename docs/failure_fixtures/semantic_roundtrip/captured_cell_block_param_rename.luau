-- Minimized from a real dump (SkyGarden BeanstalkUI): renaming a merged value
-- into a captured cell must keep the param dependency graph of the block that
-- defines it consistent (it panicked with "no entry found for key").
local log = {}
local LocalPlayer = { Character = nil }
local BeanstalkRemotes = nil
local maxRetries = 2
local ResultEvent = { handlers = {} }
function ResultEvent.OnClientEventConnect(fn)
	table.insert(ResultEvent.handlers, fn)
end
ResultEvent.OnClientEvent = { Connect = function(_, fn) ResultEvent.OnClientEventConnect(fn) end }
local FloorUnlockRequirements = { GetMaxFloor = function() return 3 end, GetAll = function() return {} end }
local BeanstalkUIDisplay = {
	IsFloorUnlocked = function(n) return n < 2 end,
	UpdateFloorLabel = function(floor, level) log[#log + 1] = tostring(floor) .. "/" .. tostring(level) end,
}
local nextFloor, itemName, GetRequirementsFunction, serverGoldNow = 2, nil, nil, 1
local displayGold, lastDisplayedGold, displayRequiredLevel = 1, 0, 4
local CurrentNextFloor = nil -- Track current next unlockable floor
if not LocalPlayer.Character then
end
local retryCount = 0
while not BeanstalkRemotes and retryCount < maxRetries do
	retryCount += 1
	if not BeanstalkRemotes then
	end
end
if not BeanstalkRemotes then
end
if ResultEvent then
	ResultEvent.OnClientEvent:Connect(function(success: boolean, message: string, floorNumber: number?)
		if success and floorNumber then
			for floorNum = floorNumber + 1, FloorUnlockRequirements.GetMaxFloor() do
				if not BeanstalkUIDisplay.IsFloorUnlocked(floorNum) then
				end
			end
			CurrentNextFloor = nextFloor
		end
	end)
end
local function countBeanstalkUnlockItems(inventory: table?): number
	if not inventory then
	end
	for _, requirement in FloorUnlockRequirements.GetAll() do
		if itemName then
		end
	end
end
local function fetchRequirementSnapshot(floorNumber: number?): (number?, number?, number?, number?)
	if not floorNumber or not GetRequirementsFunction then
	end
end
local function computeCurrentNextFloor(playerData: any): number?
	if playerData and playerData.Floors and playerData.Floors.Index then
		for floorNumber = 2, FloorUnlockRequirements.GetMaxFloor() do
			if not playerData.Floors.Index[floorNumber] then
			end
			if serverGoldNow ~= nil then
			end
			if displayGold ~= lastDisplayedGold
			then
				BeanstalkUIDisplay.UpdateFloorLabel(
					CurrentNextFloor,
					displayRequiredLevel
				)
			end
		end
		local function refreshDisplayFromCurrentConfig()
		end
	end
end
computeCurrentNextFloor({ Floors = { Index = {} } })
for _, fn in ResultEvent.handlers do
	fn(true, "", 1)
end
computeCurrentNextFloor({ Floors = { Index = {} } })
print(table.concat(log, ","))
