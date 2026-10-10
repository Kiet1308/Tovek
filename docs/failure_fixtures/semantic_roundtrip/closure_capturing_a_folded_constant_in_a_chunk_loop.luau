-- A loop of the chunk makes a literal capturing a local the loop body
-- declares with a constant (`tag`). With -g2 Luau keeps the capture, so
-- every trip makes a new closure (NEWCLOSURE), and folds the constant into
-- the body. Nothing in this chunk is shared (no DUPCLOSURE at all), which
-- the output used to read as -O0 code, where no literal is ever shared: the
-- literal printed as `function() return tag end` beside `local tag = "k"`,
-- which Luau folds and shares once compiled again from -O1 on. A folded
-- capture is itself -O1 code: the local is declared first and assigned
-- after (`local tag` / `tag = "k"`), and the literal stays new.
local made = {}
for i = 1, 2 do
	local tag = "k"
	made[i] = function()
		return tag
	end
end
print(made[1] == made[2], made[2]())
