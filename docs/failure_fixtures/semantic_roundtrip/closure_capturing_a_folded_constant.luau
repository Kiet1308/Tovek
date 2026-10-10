-- A local declared with a constant and never written, captured by a literal
-- that a function or a loop makes. From -O1 on Luau folds the constant into
-- every read. With -g2 the local keeps its register and the literal still
-- captures it (`gatherConstUpvals`), so each run makes a new closure
-- (NEWCLOSURE), but the body no longer reads the upvalue (`return tag`
-- compiles as `return 7`). Printed as `function() return 7 end`, the literal
-- captured nothing and the recompiled chunk shared one closure: `made[1] ==
-- made[2]` and `make() == make()` turned true. Such a literal reads the
-- local again where its body holds the constant (`return tag`, the index
-- key of `t[key]`), and the local is declared first and assigned after
-- (`local tag` / `tag = 7`): written, Luau captures it by reference and
-- never shares the literal. Where the body holds the constant nowhere
-- (`if DEBUG then` folded away, `size * 2` folded to 8, an exponent), the
-- literal gets `local _ = DEBUG` first: Luau captures only what the literal
-- names. An exponent keeps its constant: Luau computes `x ^ 0.5` with sqrt
-- for a constant exponent, and `1 / (-0) ^ half` would change sign.
-- `deep` captures through a literal of a literal: the middle function holds
-- the constant in a temporary register, which the inner literal captures.
-- Negative: `top` is a constant of the chunk, captured through a temporary
-- by a literal the bytecode shares (DUPCLOSURE); it stays `return 5` and
-- shared. Negative: `withParam` also captures a parameter, which keeps the
-- literal new by itself. Below -g2 the constants fold away, nothing is
-- captured, and every literal here is shared in the bytecode too.
local function make(...)
	local tag = 7
	local made = {}
	for i = 1, 2 do
		made[i] = function()
			return tag
		end
	end
	return made
end
local made = make()
print(made[1] == made[2], made[2]())

local function debugged(...)
	local DEBUG = false
	return function()
		if DEBUG then
			print("debug")
		end
		return 1
	end
end
print(debugged() == debugged(), debugged()())

local function doubled(...)
	local size = 4
	return function()
		return size * 2
	end
end
print(doubled() == doubled(), doubled()())

local function rooted(...)
	local half = 0.5
	return function(x)
		return 1 / x ^ half
	end
end
print(rooted() == rooted(), rooted()(-0), rooted()(4))

local record = {}
record.Name = "x"
local function named(...)
	local key = "Name"
	return function()
		return record[key]
	end
end
print(named() == named(), named()())

local function deep(...)
	local tag = "k"
	return function()
		return function()
			return tag
		end
	end
end
local middle = deep()
print(middle() == middle(), middle()())

local top = 5
local function shared(...)
	return function()
		return top
	end
end
print(shared() == shared(), shared()())

local function withParam(x, ...)
	local flag = true
	return function()
		return flag, x
	end
end
print(withParam(1) == withParam(1), withParam(2)())
