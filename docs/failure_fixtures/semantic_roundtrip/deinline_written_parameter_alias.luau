-- Helpers that write a parameter. Luau's -O2 copy gives such a parameter a
-- fresh register initialised with the argument; when the argument is a
-- local read nowhere after the call, SSA coalesces the two, and the copy
-- writes the caller's local itself (`value = 255 - value` on `code`). That
-- local stands for the parameter in the rebuilt call when it is dead after
-- the copy and no closure captures it (`wsign(buf, at, code, item == 1)`,
-- the temp Luau made for `item == 1` taken in as well). A helper whose copy
-- ends a loop body returns early by `continue`, which is its `return` there
-- (`add(item)`, its parameter coalesced into the loop variable). Where the
-- caller still reads its local afterwards, or a closure holds it, the copy
-- keeps a register of its own: the local never stands for the parameter
-- (`kept`, `held`).
local out = {}

local function wsign(list, at, value, negative)
	if value > 100 then
		value = bit32.band(value + 7, 127)
	end
	if negative then
		value = 255 - value
	end
	list[at] = string.format("%d", value)
end

local seen = {}
local function add(texture)
	if type(texture) == "table" then
		texture = texture.Texture
	end
	if texture == "" then
		return
	end
	seen[texture] = (seen[texture] or 0) + 1
	table.insert(out, texture)
end

local lookup = { [1] = 9, [2] = 120, [3] = 40 }

local function encode(items)
	local at = 0
	for _, item in items do
		local code = lookup[item]
		if code then
			at += 1
			wsign(out, at, code, item == 1)
		end
	end
	return at
end

local function load(list)
	for _, t in list do
		add(t)
	end
end

-- The argument is read after the copy: it keeps its value.
local function kept(n)
	local code = n * 3
	wsign(out, 10, code, false)
	return code
end

-- A closure holds the argument's local.
local function held(n)
	local code = n + 1
	local function peekCode()
		return code
	end
	wsign(out, 11, code, true)
	return peekCode()
end

print(encode({ 1, 2, 3, 4, 2 }))
print(out[1], out[2], out[3], out[4])
load({ "a", { Texture = "b" }, "", { Texture = "" }, "a" })
print(#out, out[#out], seen.a, seen.b, seen[""])
print(kept(50), out[10])
print(held(4), out[11])
