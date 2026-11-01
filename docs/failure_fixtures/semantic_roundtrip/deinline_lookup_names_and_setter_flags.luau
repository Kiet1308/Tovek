-- Calls rebuilt from copies Luau -O2 inlined, named and argued as the
-- copies read: `findChild(folder, "Drop", path)` returns the helper's
-- `parent:FindFirstChild(childName)`, so its result is `drop`, as the
-- copy's own `FindFirstChild("Drop")` named it, not the helper's generic
-- `child`. A setter whose flag only picks a value (`flag and 0 or 1`)
-- keeps the `false` its copy folded, not a left-out `nil`.
local seen = {}
local function warnOnce(key, ...)
	if seen[key] then
		return
	end
	seen[key] = true
	print("warn", ...)
end

local function warnMissing(path)
	warnOnce("missing:" .. path, "[FX] missing object:", path)
end

local function findChild(parent, childName, path)
	local child = parent:FindFirstChild(childName)
	if not child then
		warnMissing(path)
	end
	return child
end

local function setHighlight(parent, flag)
	local stroke = parent:FindFirstChild("Highlight")
	if not stroke then
		stroke = { Name = "Highlight" }
		parent.children.Highlight = stroke
	end
	stroke.Transparency = flag and 0 or 1
end

local function node(name)
	local children = {}
	local self = { Name = name, children = children }
	function self:FindFirstChild(childName)
		return children[childName]
	end
	return self
end

local function getDropTemplate(folder)
	local drop = findChild(folder, "Drop", "Assets/Drop")
	if not drop then
		return nil
	end
	local attachment = findChild(drop, "WaterDropAttachment", "Assets/Drop/WaterDropAttachment")
	if not attachment then
		return nil
	end
	return attachment
end

local function toggle(current, previous)
	if previous then
		setHighlight(previous, false)
	end
	setHighlight(current, true)
end

local folder = node("Folder")
folder.children.Drop = node("Drop")
print(getDropTemplate(folder), getDropTemplate(node("Empty")))
folder.children.Drop.children.WaterDropAttachment = node("Attachment")
print(getDropTemplate(folder).Name)
local a, b = node("A"), node("B")
toggle(a, nil)
toggle(b, a)
print(a.children.Highlight.Transparency, b.children.Highlight.Transparency)
