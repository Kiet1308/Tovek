-- A void helper choosing one store by class (`if IsA("Model") then ...
-- elseif ...`): its canonical body is un-guarded into the negated form
-- (`if not IsA("Model") then ... else PivotTo end`) while the copies Luau
-- -O2 inlined keep the positive one. Value helpers already matched either
-- polarity; void ones now do too, and each copy is a call again.
local function makeObject(class)
	local object = { Class = class, Log = {} }
	function object:IsA(name)
		return self.Class == name
	end
	function object:PivotTo(cframe)
		table.insert(self.Log, "pivot " .. cframe)
	end
	return object
end

local function positionClone(instance, cframe)
	if instance:IsA("Model") then
		instance:PivotTo(cframe)
	elseif instance:IsA("BasePart") then
		instance.CFrame = cframe
	elseif instance:IsA("Attachment") then
		instance.WorldCFrame = cframe
	end
end

local function place(instance, cframe, folder)
	positionClone(instance, cframe)
	instance.Parent = folder
	positionClone(instance, cframe .. "!")
end

for _, class in { "Model", "BasePart", "Attachment", "Other" } do
	local object = makeObject(class)
	place(object, "cf", "folder")
	print(class, table.concat(object.Log, ","), object.CFrame, object.WorldCFrame, object.Parent)
end
