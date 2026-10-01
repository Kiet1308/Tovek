-- With `setfenv`, a function can resolve globals in its own environment:
-- the same `print("tag")` in a helper and in its caller do different
-- things. Luau neither inlines nor imports in such a module.
local function helper()
	print("tag")
end
setfenv(helper, { print = function(...) print_real("hooked", ...) end })
print_real = print
print("tag")
helper()
