-- A helper testing a parameter for truth and handing it to a function
-- literal (`if not model then return end; pcall(function() callback(model)
-- end)`). Luau -O2 inlines it with the caller's argument; where the caller's
-- own `if child then` guards the copy, or its `if` folds into the
-- caller's, the literal is left alone in the arm, still capturing `child`. The de-inliner used to read
-- that as the helper with the constant `true` (`play(act, true)`), passing
-- `true` where the literal must get `child`. The constant now stands only
-- where nothing the specialization kept reads the argument; the copy here
-- is the whole `if`, rebuilt as `play(act, child)`.
-- A call with `nil` (`show(nil)`) is rebuilt with the argument left out.
local log = {}
local function play(callback, model)
	if not model then
		return
	end
	pcall(function()
		callback(model, #log)
	end)
end
local function act(m, n)
	table.insert(log, "act " .. tostring(m) .. " " .. n)
end
local function show(child)
	play(act, child)
	return #log
end
local function guarded(child)
	if child then
		play(act, child)
	end
	return #log
end
print(show("left"), show(nil), guarded("right"), guarded(false))
print(table.concat(log, ";"))
