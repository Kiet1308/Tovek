-- A rebuilt helper call names its result after the local the helper
-- returns (`local track = loadTrack(...)`), as the inlined copy did.
local Animator = {}
Animator.__index = Animator

function Animator:LoadAnimation(animation)
	return { Animation = animation, Playing = false }
end

local function loadTrack(animator, id, looped)
	local animation = { AnimationId = id }
	local track = animator:LoadAnimation(animation)
	track.Looped = looped
	return track
end

local function setup(animator)
	local idle = loadTrack(animator, "rbxassetid://507766666", true)
	local swing = loadTrack(animator, "rbxassetid://522635514", false)
	idle.Playing = true
	return idle, swing
end

local idle, swing = setup(setmetatable({}, Animator))
print(idle.Animation.AnimationId, idle.Looped, idle.Playing)
print(swing.Animation.AnimationId, swing.Looped, swing.Playing)
