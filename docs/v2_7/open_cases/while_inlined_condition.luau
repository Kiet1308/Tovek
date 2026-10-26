-- Readability edge case (landing sample, V2.6 tip e938654, -O2 -g1; rechecked on 1aeabd8).
-- a(): `while isAlive(h) do task.wait(frames(1)) end; award(-25)` still prints as
--      `while true do if not isAlive(p) then break end ... end` (V2.6 also moved `award` into an else; fixed).
-- b(): `repeat ... until not isAlive(h)` prints as written since c864e16.
-- c(): a guarded `while` with `break` prints as written. Behaviour is the same; only the shape of a() is worse.
local function frames(count)
	return count / 60
end
local function isAlive(humanoid)
	return humanoid ~= nil and humanoid.Health > 0
end
local function a(humanoid, award)
	while isAlive(humanoid) do
		task.wait(frames(1))
	end
	award(-25)
end
local function b(humanoid, award)
	repeat
		task.wait(frames(1))
	until not isAlive(humanoid)
	award(-25)
end
local function c(humanoid, lamp)
	local started = os.clock()
	while os.clock() - started < 5 do
		if not isAlive(humanoid) then
			break
		end
		lamp.Transparency = (os.clock() - started) / 5
		task.wait(frames(1))
	end
	lamp.Transparency = 0
end
return {a, b, c}
