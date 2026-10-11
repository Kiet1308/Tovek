-- A local declared before a loop and captured inside it is one cell for
-- every iteration: the bytecode captures its register by reference and
-- closes it only after the loop. When the body writes the register before
-- any read, the value it holds at the loop entry is dead: no loop-header phi
-- exists, SSA sees only the versions written inside the loop, and the
-- variable was declared in the body, one cell per iteration (fr-core F1, in
-- V2.5.1 too): `collect` printed `local name = item.name` inside the loop and
-- returned `x y` for `y y`. The definitions reaching the loop now join the
-- cell whatever their kind: an initializer (`collect`, a closure in
-- `replaced`), a parameter (`param`), an enclosing `for` loop's variable
-- (`outerVar`), a merge (`merged`), a multiple assignment (`multi`, whose
-- call also reads the register: only the written value joins).
local function show(handlers)
	local out = {}
	for i, handler in ipairs(handlers) do
		out[i] = tostring(handler())
	end
	return table.concat(out, ",")
end

local function collect(items)
	local handlers = {}
	local label = ""
	for _, item in items do
		label = item.name
		handlers[#handlers + 1] = function()
			return label
		end
	end
	return handlers
end
print(show(collect({ { name = "x" }, { name = "y" } })))

local function param(a, b)
	local handlers = {}
	for i = 1, 2 do
		a = b + i
		handlers[#handlers + 1] = function()
			return a
		end
	end
	return handlers
end
print(show(param(1, 2)))

local rows = { 3, 1 }
local function outerVar(a, b)
	local handlers = {}
	for k, w in ipairs(rows) do
		for i = 0, 1, 0.5 do
			k = if i >= w or 0.5 <= b then (if a > 0.5 then -1 else w) else #rows
			handlers[#handlers + 1] = function()
				k += 1
				return k
			end
		end
	end
	return handlers
end
print(show(outerVar(1, 2)), show(outerVar(0, 0)))

local function replaced(a)
	local handlers = {}
	local f = function(p)
		return p + a
	end
	for i = 1, 2 do
		f = a * i
		handlers[#handlers + 1] = function()
			f += 1
			return f
		end
	end
	return handlers
end
print(show(replaced(10)))

local function merged(c, items)
	local label
	if c then
		label = "a"
	else
		label = "b"
	end
	print(label)
	local handlers = {}
	for _, item in items do
		label = item
		handlers[#handlers + 1] = function()
			return label
		end
	end
	return handlers
end
print(show(merged(true, { "x", "y" })), show(merged(false, { "z" })))

local function multi()
	local handlers = {}
	local a, b = string.byte("AB", 1, 2)
	for i = 1, 2 do
		a = i + b
		handlers[#handlers + 1] = function()
			return a
		end
	end
	return handlers
end
print(show(multi()))

local function repeatDead()
	local handlers = {}
	local x = 100
	local n = 0
	repeat
		n += 1
		x = n * 2
		handlers[#handlers + 1] = function()
			return x
		end
	until n >= 3
	return handlers
end
print(show(repeatDead()))

-- Negative: a local declared inside the loop is closed every iteration and
-- stays a cell per iteration; an outer loop closes `cell` per outer
-- iteration while it stays open around the inner loop.
local function perIteration(items)
	local handlers = {}
	for _, item in items do
		local v = item
		handlers[#handlers + 1] = function()
			v ..= "!"
			return v
		end
	end
	return handlers
end
local each = perIteration({ "x", "y" })
print(each[1](), each[1](), each[2]())

local function nested()
	local handlers = {}
	for o = 1, 2 do
		local cell = 0
		for i = 1, 2 do
			cell = o * 10 + i
			handlers[#handlers + 1] = function()
				cell += 100
				return cell
			end
		end
	end
	return handlers
end
print(show(nested()))
