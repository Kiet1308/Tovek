-- A helper returning a function literal that captures its parameter, which
-- Luau -O2 inlines where the caller passes a constant (`bind("y")`). The copy
-- captures the constant through a temporary register, and each run of it
-- makes a new closure (NEWCLOSURE). Where the copy stays inline (in `make`
-- with -g2, and everywhere with -g0, where `bind` itself is gone), it prints
-- as a local holding the constant that the literal captures. Compiled again,
-- Luau folds such a local into the literal, which then captures nothing and
-- loads one shared closure (DUPCLOSURE): two runs of `make` would return
-- equal closures, and the loop would store one key instead of three. The
-- output declares that local first and assigns it after: a written local is
-- captured by reference, and Luau never shares such a literal.
-- Negative: `shared` captures nothing; its copy is one object in the bytecode
-- too, and stays as it is.
-- Negative: `label`'s literal captures the parameter `p`, which Luau never
-- shares either; nothing changes there.
-- A top-level local captured both by the copy of `bind` in a loop (`made`,
-- a new object each run) and by a literal the bytecode shares (`kept`,
-- DUPCLOSURE: one object). Splitting its declaration would make `kept` new
-- on every run too. The copy captures a private copy of the local instead
-- (`local name2` / `name2 = name`, right before it), and `kept` stays one
-- object.
local function bind(tag)
	return function()
		return tag
	end
end

local function shared()
	return function()
		return 1
	end
end

local function make()
	return bind("y"), shared()
end

local function label(p)
	local function show()
		return p
	end
	return show
end

local a, s1 = make()
local b, s2 = make()
print(a == b, a(), b(), s1 == s2)

local list = { 10, 20, 30 }
local keys = {}
local count = 0
for i = 1, #list do
	keys[bind(true)] = list[i]
end
for _ in keys do
	count += 1
end
print(count)

local l1, l2 = label(1), label(1)
print(l1 == l2, l1(), l2())

local name = tostring(#list + 7)
local made, kept = {}, {}
for i = 1, 2 do
	made[i] = bind(name)
	kept[i] = function()
		return name
	end
end
print(made[1] == made[2], kept[1] == kept[2], made[1](), kept[2]())
