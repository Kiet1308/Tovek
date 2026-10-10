-- A rebuilt call takes in the declarations Luau made for its arguments
-- right before the copy (`place(part, i * gap + 1)`, a register with no
-- debug name, or the one of the parameter it fills), but not a service or
-- module handle (D4): the SSA inliner keeps those declared wherever they
-- stand, so `local Lighting = game:GetService("Lighting")` stays beside
-- `setup(Lighting, n)`, and the handle `configure2` passes is declared the
-- same way. (A local with a debug name of its own stays declared too.)
local log = {}
local services = {
	Lighting = { name = "Lighting" },
	Workspace = { name = "Workspace" },
}
local game = {
	GetService = function(_, name)
		table.insert(log, "get " .. name)
		return services[name]
	end,
}

local function setup(s, n)
	s.Brightness = n
	s.ClockTime = n * 2
	s.FogEnd = n * 100
	return s
end

local function configure(n)
	local Lighting = game:GetService("Lighting")
	setup(Lighting, n)
end

local function configure2(n)
	setup(game:GetService("Workspace"), n + 1)
end

local function place(part, offset)
	part.X = offset
	part.Y = offset * 2
	part.Z = offset + part.X
end

local function layout(parts, gap)
	for i, part in parts do
		local step = i * gap + 1
		place(part, step)
	end
end

local function layout2(parts, gap)
	for i, part in parts do
		place(part, i * gap + 1)
	end
end

configure(2)
configure2(3)
local parts = { {}, {} }
layout(parts, 2)
print(parts[1].X, parts[2].Z)
layout2(parts, 3)
print(parts[1].Y, parts[2].Z)
print(services.Lighting.Brightness, services.Lighting.ClockTime, services.Lighting.FogEnd)
print(services.Workspace.Brightness, services.Workspace.FogEnd)
print(table.concat(log, ", "))
