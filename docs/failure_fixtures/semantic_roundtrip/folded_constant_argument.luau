-- Luau folds the reads of a constant argument whose truth alone is tested
-- (`visible and 0 or 1` with `false` is `1`, `parent or root` with nothing
-- passed is `root`), so the inlined copy keeps no trace of it. The rebuilt
-- call passes the constant that specializes the helper into exactly the copy.
local log = {}

local function fade(frame, visible)
	local goal = { Transparency = visible and 0 or 1 }
	frame.Goal = goal
	table.insert(log, frame.Name .. "=" .. goal.Transparency)
	return goal
end

local root = { Name = "root" }

local function create(className, name, parent)
	local instance = { ClassName = className, Name = name }
	instance.Parent = parent or root
	table.insert(log, name .. " in " .. instance.Parent.Name)
	return instance
end

local function open(menu)
	menu.Visible = true
	fade(menu.Background, true)
	fade(menu.Shadow, true)
end

local function close(menu)
	local goal = fade(menu.Background, false)
	fade(menu.Shadow, false)
	menu.Visible = false
	return goal.Transparency
end

local function build()
	local remote = create("RemoteEvent", "Event")
	local folder = create("Folder", "Commands")
	create("Folder", "Types", folder)
	return remote.Parent == root, folder.Name
end

local menu = { Background = { Name = "background" }, Shadow = { Name = "shadow" } }
open(menu)
print(close(menu), menu.Visible)
print(build())
print(table.concat(log, ", "))
