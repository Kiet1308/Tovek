-- Calls the de-inliner rebuilds carry their comment as an attribute, not as a
-- marker statement in the tree. The marker statement used to sit between
-- `if p then` and the rebuilt `if inRoots(x) then`, so the two ifs could not
-- merge back into the source's `if p and inRoots(x) then`. Now every rebuilt
-- call ends its line with `-- inferred equivalent call` (a header line for an
-- `if`, one comment for the two calls in one expression), and each helper's
-- definition line states how many calls the output holds. `scaled` is matched
-- inside an expression, `emit` as statements, `inRoots` as conditions.
local function node(name, parent)
	return {
		Name = name,
		Size = { X = #name },
		attributes = {},
		IsDescendantOf = function(self, other)
			local at = self.parent
			while at do
				if at == other then
					return true
				end
				at = at.parent
			end
			return false
		end,
		SetAttribute = function(self, key, value)
			self.attributes[key] = value
		end,
		GetAttribute = function(self, key)
			return self.attributes[key]
		end,
		parent = parent,
	}
end

local world = node("World")
local storage = node("Storage")
storage:SetAttribute("Bias", 4)
local roots = { world, storage }

local function inRoots(x)
	for _, root in ipairs(roots) do
		if x:IsDescendantOf(root) then
			return true
		end
	end
	return false
end

local log = {}

local function emit(part, n)
	part:SetAttribute("Count", n)
	log[#log + 1] = part.Name .. "=" .. tostring(n)
end

local function scaled(part, k)
	return part.Size.X * k + roots[2]:GetAttribute("Bias")
end

local function run(p, part, items)
	if p and inRoots(part) then
		print("inside", part.Name)
		return
	end
	for i, item in items do
		emit(item, i)
	end
	emit(part, 0)
	if inRoots(items[1]) then
		emit(items[1], 10)
	end
	print(scaled(part, 2) + scaled(part, 3))
end

local inside = node("Inside", world)
local outside = node("Outside")
local loose = node("Loose", storage)

run(true, inside, { outside })
run(true, outside, { inside, loose })
run(false, inside, { loose })
print(table.concat(log, " "), inside:GetAttribute("Count"), loose:GetAttribute("Count"))
