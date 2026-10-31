-- A module table Luau sizes from the field stores after `local M = {}`
-- (17 fields: a 32-node hash part). The SSA fold takes the leading stores
-- into the constructor, which then needs `nil` fields to keep that
-- allocation; as stores again they print as written, sized the same.
local Product = {}
Product.DisplayName = "Premium"
Product.Description = "Exclusive benefits"
Product.Effects = { Boost = 25 }
Product.Information = { `+{Product.Effects.Boost}% faster` }
Product.Type = "Product"
Product.Rarity = "Mythic"
Product.Icon = "rbxassetid://1"
Product.Rewards = {}
Product.Unobtainable = true
function Product.describe()
	return Product.DisplayName .. ": " .. Product.Description
end
function Product.boost(value)
	return value * (1 + Product.Effects.Boost / 100)
end
Product.A = 1
Product.B = 2
Product.C = 3
Product.D = 4
Product.E = 5
Product.F = 6

local keys = {}
for key in pairs(Product) do
	table.insert(keys, key)
end
print(table.concat(keys, ","))
print(Product.describe(), Product.boost(10))
