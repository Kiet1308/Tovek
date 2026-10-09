-- Helpers that end by returning an outer local (`return ignoreList`): called
-- for no result their copy is the body without that `return`, and the
-- statement right after it reads the local first of all it evaluates. That
-- read gets the value the call returns, the one the copy left there, so the
-- call stands in its place: a method receiver (`getTrack(h):Play()`), a
-- declaration, a field read, and the last argument of a call, which takes
-- all the results of a helper giving exactly one.
-- Negative: `track:Stop()` after `stopAll()`, which ends `return nil`:
-- another helper, its call stays a statement.
local log = {}
local animator = { LoadAnimation = function(_, name) return { name = name, Play = function(self) table.insert(log, "play " .. self.name) end } end }
local track = nil
local function getTrack(a)
	if a ~= nil then
		track = a:LoadAnimation("fail")
		assert(track, "")
		track.Looped = false
	end
	return track
end

local ignoreList = nil
local function getIgnoreList()
	if ignoreList then
		return ignoreList
	end
	ignoreList = {}
	assert(ignoreList, "")
	table.insert(ignoreList, "character")
	return ignoreList
end

local function stopAll()
	if track then
		track.stopped = true
		table.insert(log, "stop")
	end
	return nil
end

local world = {}
function world:find(ray, list)
	return ray .. "/" .. table.concat(list, ",")
end

local M = {}
function M.play(h)
	if h then
		getTrack(h):Play()
	end
	return track and track.Looped
end
function M.cast(ray)
	return world:find(ray, getIgnoreList())
end
function M.count()
	local list = getIgnoreList()
	return #list
end
function M.first()
	return getIgnoreList()[1]
end
function M.stop()
	stopAll()
	return track and track.stopped
end

print(M.play(animator), M.play(nil))
print(M.cast("r1"), M.count(), M.first())
print(M.stop())
print(table.concat(log, ";"))
