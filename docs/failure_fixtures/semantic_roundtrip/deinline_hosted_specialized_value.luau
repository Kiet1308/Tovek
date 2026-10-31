-- Copies Luau -O2 made of `openGifts(amount)` inside table constructors,
-- each with a constant amount: the branch on `amount ~= 1` is decided, and
-- SSA then puts the text the local holds where it is read (`"Gift Box"`,
-- or `"Gift Box" .. "es"` as `"Gift Boxes"`), so a copy is the returned
-- table alone, in place. Specialized and propagated alike, each is
-- `openGifts(n)` again.
local function openGifts(amount)
	local noun = "Gift Box"
	if amount ~= 1 then
		noun ..= "es"
	end
	return {
		Type = "Have Items",
		Amount = amount,
		Item = "GiftBox",
		Description = "Open " .. amount .. " " .. noun .. " around the map.",
	}
end

local quests = {}
local function push(quest)
	table.insert(quests, quest)
end

push({ Name = "First", Tasks = { { Type = "Collect", Amount = 10 }, openGifts(1) } })
push({ Name = "Second", Tasks = { openGifts(2), { Type = "Defeat", Amount = 3 } } })
push({ Name = "Third", Tasks = { { Type = "Collect", Amount = 20 }, openGifts(5) } })

for _, quest in quests do
	for _, task in quest.Tasks do
		print(quest.Name, task.Type, task.Amount, task.Description)
	end
end
