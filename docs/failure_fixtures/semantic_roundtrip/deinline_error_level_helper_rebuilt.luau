-- `error(message, level)` prefixes a string message with the position of the
-- frame `level` up: level 2 is the caller of the function calling `error`.
-- A rebuilt helper call adds a frame, so that position moves: written out in
-- `setSpeed`, level 2 is the caller of `setSpeed` (`pcall`, a C function: no
-- position); in a call `checkNumber(v, "speed")`, it is `setSpeed`. Positions
-- are not part of what a decompile keeps (no output line keeps its number
-- either), so the frame gate no longer refuses such helpers (it did; TestEZ's
-- assertLevel and trove's assertions stayed written out): the copies may be
-- rebuilt, and only the message text has to match, here stripped of its
-- position. Real frame reads (`debug.info`) still refuse a helper.
local function checkNumber(v, name)
	if type(v) ~= "number" then
		error("bad " .. name, 2)
	end
	print("ok", name)
end

local function setSpeed(v)
	if type(v) ~= "number" then
		error("bad " .. "speed", 2)
	end
	print("ok", "speed")
	return v * 2
end

local function setAngle(v)
	checkNumber(v, "angle")
	return v / 2
end

local function need(x, name)
	if x == nil then
		error("missing " .. name, 1)
	end
	print("have", name)
end

local function setLabel(x, label)
	need(x, label)
	return x
end

local function setTitle(x, label)
	need(x, label)
	return label
end

local function checkName(v)
	if type(v) ~= "string" then
		_G.error("bad name", 2)
	end
	print("name", v)
end

local function setName(v)
	if type(v) ~= "string" then
		_G.error("bad name", 2)
	end
	print("name", v)
	return #v
end

local function needName(v)
	return v or _G["error"]("no name", 2)
end

local function nameOf(v)
	local name = v or _G["error"]("no name", 2)
	print("named", name)
	return name
end

local function text(message)
	return (string.gsub(message, "^.-:%d+: ", ""))
end

checkNumber(4, "count")
print(setSpeed(3), setAngle(8), setLabel(1, "label"), setTitle(2, "title"))
local ok, message = pcall(setSpeed, "fast")
print(ok, text(message))
ok, message = pcall(setAngle, "wide")
print(ok, text(message))
ok, message = pcall(setLabel, nil, "label")
print(ok, text(message))
checkName("first")
print(setName("ab"), needName("cd"), nameOf("ef"))
ok, message = pcall(setName, 5)
print(ok, text(message))
ok, message = pcall(nameOf, false)
print(ok, text(message))
