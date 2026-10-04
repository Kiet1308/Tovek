-- An upvalue operand is fetched before a later call operand runs:
-- `x + change()` adds the old `x`. A recovered `helper(change())` would run
-- `change()` first and read the new one, in a statement or an expression.
-- (`pcall` keeps the compiler from inlining the callers into the chunk,
-- where `x` would be a register read after the call.)
local x = 1

local function change()
	x = 2
	return 0
end

local function helper(p)
	print("value", x + p)
end

local function caller()
	print("value", x + change())
end

pcall(caller)

local y = 1

local function changeY()
	y = 3
	return 2
end

local function scaled(p)
	return y * 3 + p * 2 - 1
end

local function scaledCaller()
	return y * 3 + changeY() * 2 - 1, 0
end

print(pcall(scaledCaller))
return helper, scaled
