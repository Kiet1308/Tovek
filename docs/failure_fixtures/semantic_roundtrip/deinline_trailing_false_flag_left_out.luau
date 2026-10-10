-- A constant flag Luau -O2 folded away, passed last as `false` (plan E2,
-- rule (c)). The statement de-inliner infers the flag from the copy's
-- shape. Where the flag only switches extra work on (every read is `if
-- flag then ... end`, no else) and `nil` gives the same copy, the argument
-- is left out, as such an option is usually written: `flash(a)`. Kept:
-- `flash(b, true)` (true); `setVisible(c, false)` and `toggle(c, false)`
-- (a setter's `false` is what a reader expects: `visible and 0 or 1`,
-- `if on then ... else ... end`); `tag(t, false)` (the body hands the flag
-- on: `flag and 1` is `false` for false and `nil` for nil); `mark(false,
-- c)` (not the last parameter). `Copy(t, false)` stays `table.clone(t)`: a
-- copy no larger than the call is never claimed for a constant.
local log = {}
local function record(...)
	local parts = {}
	for i = 1, select("#", ...) do
		parts[i] = tostring((select(i, ...)))
	end
	table.insert(log, table.concat(parts, " "))
end

local function flash(p, keep)
	p.Color = "red"
	p.Size = 2
	if keep then
		p.Keep = true
		record("kept", p.Name)
	end
	record("flash", p.Name)
end

local function Copy(t, deep)
	if not deep then
		return table.clone(t)
	end
	local copy = {}
	for k, v in t do
		copy[k] = if type(v) == "table" then Copy(v, true) else v
	end
	return copy
end

local function tag(t, flag)
	t.value = flag and 1
	t.tagged = true
	record("tag", t.value)
end

local function setVisible(p, visible)
	p.Shown = true
	p.Transparency = visible and 0 or 1
	record("visible", p.Transparency)
end

local function toggle(p, on)
	if on then
		p.State = "on"
		record("on", p.Name)
	else
		p.State = "off"
		record("off", p.Name)
	end
end

local function mark(flag, p)
	if flag then
		p.Marked = true
	end
	p.Seen = true
	record("mark", p.Seen, p.Marked)
end

local function main()
	local a, b, c = { Name = "a" }, { Name = "b" }, { Name = "c" }
	flash(a, false)
	flash(b, true)
	local t = { x = 1, y = { z = 2 } }
	local shallow = Copy(t, false)
	local deep = Copy(t, true)
	record(shallow.y == t.y, deep.y == t.y, deep.y.z)
	tag(c, false)
	mark(false, c)
	setVisible(c, false)
	toggle(c, false)
	record(a.Keep, b.Keep, c.value)
end

main()
for _, line in log do
	print(line)
end
