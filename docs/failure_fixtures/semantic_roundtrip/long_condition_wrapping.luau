-- A long `and`/`or` condition printed on one line (up to 352 columns in the
-- samples). One that runs past the line now wraps StyLua-style: the keyword
-- alone, one operand per line led by its operator, `then`/`do` alone, the
-- same for `elseif`, `while` and `until`. An assigned or returned chain
-- hangs its operators one level deeper, and an `and` chain under an `or`
-- that still runs past the line continues the same way. Only whitespace
-- changes: operand order, short-circuiting and parentheses stay, as the
-- trace of evaluated operands shows. `probe` is global, so -O2 cannot
-- inline it away.
local trace = {}
function probe(name, value)
	table.insert(trace, name)
	return value
end

local function classify(state)
	if probe("falling", state == "FallingDown") or probe("freefall", state == "Freefall") or probe("jumping", state == "Jumping") then
		return "air"
	elseif probe("walking", state == "Walking") and probe("grounded", state ~= "Seated") and probe("ready", state ~= nil) then
		return "ground"
	end
	local fallback = probe("first fallback", state == "Dead") and probe("second fallback", "dead state reached")
		and probe("third fallback", "still dead") or probe("final fallback", "unknown state")
	return fallback
end

local function countdown(n)
	local steps = 0
	while probe("positive", n > 0) and probe("budget", steps < 100) and probe("not stopped", n ~= 3) and probe("always", true) do
		n -= 1
		steps += 1
	end
	repeat
		n += 1
	until probe("reached the target", n >= 5) or probe("overflow guard tripped", n > 1000) or probe("never reached", n == -12345)
	return steps, n
end

local function ready(player)
	return player ~= nil and player.Character ~= nil and player.Character.Humanoid ~= nil and player.Character.Humanoid.Health > 0
end

for _, state in { "Freefall", "Walking", "Dead", "Seated" } do
	print(classify(state), table.concat(trace, ","))
	table.clear(trace)
end
print(countdown(6), table.concat(trace, ","))
print(ready(nil), ready({ Character = { Humanoid = { Health = 5 } } }))
