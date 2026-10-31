-- Copies Luau -O2 made of a helper returning a function literal that
-- captures its parameters and writes some of them: each copy declares a
-- local per captured parameter (`local name = nil`) and builds the literal
-- inside a table constructor. Each is its call again, with no trailing
-- `nil`s (`makeCutscene("House")`).
local toys = {
	House = { CameraTarget = "door", Pos2 = "roof" },
	Tree = { CameraTarget = "trunk", Pos2 = "top" },
}

local function makeCutscene(toyName, zoom, duration, posName, targetName)
	return function()
		local toy = toys[toyName]
		if not targetName then
			targetName = "CameraTarget"
		end
		local target = toy[targetName]
		if not posName then
			posName = "CameraPos"
		end
		return target, posName, zoom or 1, duration or 99
	end
end

local Quests = {}
function Quests.Init()
	return {
		Start = { Say = { "Hi", { Text = "Look", Cutscene = makeCutscene("House") } } },
		Finish = { Say = { { Text = "Bye", Cutscene = makeCutscene("Tree", 2, nil, "Pos2") } } },
	}
end

local quest = Quests.Init()
print(quest.Start.Say[2].Cutscene())
print(quest.Finish.Say[1].Cutscene())
print(quest.Finish.Say[1].Cutscene())
