-- A module table Luau sized for its fields and its methods (NEWTABLE hash
-- 32), whose data fields fold into its constructor and whose methods,
-- defined after a call, stay stores. One field is a constructor with
-- explicit array keys and one record key (`SellRewards` in a real game),
-- NEWTABLE hash 1, array 5: the `[n]` keys land in the array part. The hash
-- census counted them, so printing that field (to check which names the
-- module's fields read) failed its own allocation check and the whole
-- script was refused.
local Portals = {}
Portals.List = {}
Portals.MaxTier = 5
Portals.TierRarities = { "Common", "Rare", "Epic", "Legendary", "Mythic" }
Portals.SellRewards = {
	[1] = { { Asset = "Gold", Amount = 25 } },
	[2] = { { Asset = "Gold", Amount = 50 } },
	[3] = { { Asset = "Gold", Amount = 100 } },
	[4] = { { Asset = "Gold", Amount = 500 } },
	[5] = { { Asset = "Gold", Amount = 1000 } },
	Secret = { { Asset = "Gold", Amount = 1000 } },
}
Portals.AugmentChanceBoosts = { 20, 40, 60 }
print("methods")
function Portals.Method0(value)
	return value + 0
end
function Portals.Method1(value)
	return value + 1
end
function Portals.Method2(value)
	return value + 2
end
function Portals.Method3(value)
	return value + 3
end
function Portals.Method4(value)
	return value + 4
end
function Portals.Method5(value)
	return value + 5
end
function Portals.Method6(value)
	return value + 6
end
function Portals.Method7(value)
	return value + 7
end
function Portals.Method8(value)
	return value + 8
end
function Portals.Method9(value)
	return value + 9
end
function Portals.Method10(value)
	return value + 10
end
function Portals.Method11(value)
	return value + 11
end
local keys = {}
for key, value in pairs(Portals.SellRewards) do
	table.insert(keys, tostring(key) .. "=" .. value[1].Amount)
end
table.sort(keys)
print(table.concat(keys, ","), #Portals.SellRewards, Portals.MaxTier)

