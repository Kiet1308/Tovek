-- Copies Luau -O2 made of a helper opening with a store into its table
-- parameter (`set.Name = name`): the copy's first store follows the
-- argument's constructor, and the SSA fold takes it in as the constructor's
-- last entry (`local honey = {..., Name = "Honey"}`). Each copy is its call
-- again with the constructor as it was written; a table no helper stores
-- into keeps its entries.
local byName = {}
local list = {}

local function pushSet(name, set)
	set.Name = name
	byName[name] = set
	if not set.Count then
		set.Count = 4
	end
	table.insert(list, set)
end

pushSet("Honey", {
	Count = 5,
	Task = { Type = "Make Honey", Amount = 1000000 },
	Bonus = { Type = "Rate", Value = { 1.3, 1.6, 1.9 }, Op = "Mul" },
	BonusName = "Convert Rate",
	BonusLabels = { "x1.3", "x1.6", "x1.9" },
	BadgeIDs = { 1749468648, 1749519033, 1749523673 },
})
pushSet("Quest", {
	Task = { Type = "Complete Quests", Amounts = { 10, 50, 250 } },
	Bonus = { Type = "Capacity", Value = { 1.25, 1.5 }, Op = "Mul" },
})
local state = { moving = false }
state.Name = "plain"
print(#list, byName.Honey.Count, byName.Quest.Count, list[2].Name, state.Name)
