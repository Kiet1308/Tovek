-- Copies Luau -O2 made of local helpers that stayed inlined: an argument
-- whose closure writes a local of the caller (captured by reference, so the
-- call may take it), a flag folded to `false` that leaves `if not v then
-- return end` (`if v` once canonical), and results stored into locals the
-- caller already has, the values nothing takes still evaluated.
local handles = {}
local function track(handle)
	table.insert(handles, handle)
	return handle
end

local function watch(name)
	local stopped = false
	track({
		Disconnect = function()
			stopped = true
		end,
	})
	handles[#handles].Disconnect()
	print(name, stopped)
end
watch("a")
watch("b")

local function makeToggle()
	local current = nil
	local function update(flag)
		if flag and not current then
			current = { alive = true }
			return
		end
		if flag or not current then
			return
		end
		current.alive = false
		current = nil
	end
	return function(on, hidden)
		if hidden then
			update(false)
		else
			update(on)
		end
		return current ~= nil
	end
end
local toggle = makeToggle()
print(toggle(true, false), toggle(true, true), toggle(false, false), toggle(true, false))

local function basis(x, y)
	local u = x * y
	if u < 0 then
		u = -u
	end
	local v = u * 2
	return v, tostring(v) .. "!"
end
local p, q = 0, ""
for i = 1, 2 do
	p = basis(i, 3)
	print(p, q)
	p, q = basis(-i, i + 1)
	print(p, q)
end
print(p, q)
local r, s = basis(4, 5)
print(r)
