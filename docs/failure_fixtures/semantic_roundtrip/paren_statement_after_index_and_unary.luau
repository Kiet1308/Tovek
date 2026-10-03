-- A statement printed starting with `(` after a line ending in a name, `)`
-- or `]` must be separated by `;`: nested index targets (`(a or b).x.y = 3`,
-- as in Zone+ `bindToGroup`) and unary tails (`#value`) were missed.
local Zone = {}
Zone.__index = Zone
local groups = {}
local module = {}
function module.getGroup(name) return groups[name] end
function module.setGroup(name) local group = {_memberZones = {}} groups[name] = group return group end
function Zone:unbindFromGroup() self.unbound = true end
function Zone:bindToGroup(settingsGroupName)
	self:unbindFromGroup();
	(module.getGroup(settingsGroupName) or module.setGroup(settingsGroupName))._memberZones[self.zoneId] = self
	self.settingsGroupName = settingsGroupName
end
local zone = setmetatable({zoneId = 7}, Zone)
zone:bindToGroup("g")
print(groups.g._memberZones[7] == zone, zone.unbound, zone.settingsGroupName)
local a, b = nil, {x = {}}
local function f() end
f(); (a or b).x.y = 3
print(b.x.y)
local value = "abc"
local n = #value;
(a or b).x.z = n
print(b.x.z)
