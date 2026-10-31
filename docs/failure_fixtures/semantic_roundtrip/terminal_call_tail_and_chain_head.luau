-- A shared `return` holding a call stays one statement after the `if`
-- instead of a guard duplicating it, and a value read once as the head of
-- the returned `or` folds into it (`return hooks:After(self) or
-- self.Response`, `return tonumber(text:match("(%d+)$")) or math.huge`).
local Command = {}
Command.__index = Command
function Command:Run()
	if self.Response == nil then
		if self.Handler then
			self.Response = self.Handler(self)
		elseif self.Fallback then
			print(self.Name, "fell back")
			self.Response = "fallback"
		else
			print(self.Name, "has no implementation")
			self.Response = "No implementation."
		end
	end
	return self.Hooks:After(self) or self.Response
end

local function suffixNumber(text)
	return tonumber(text:match("(%d+)$")) or math.huge
end

local hooks = {
	After = function(_, command)
		return nil
	end,
}
print(setmetatable({ Name = "a", Hooks = hooks }, Command):Run())
print(setmetatable({ Name = "b", Hooks = hooks, Handler = function() return "ran" end }, Command):Run())
print(suffixNumber("item12"), suffixNumber("item"))
