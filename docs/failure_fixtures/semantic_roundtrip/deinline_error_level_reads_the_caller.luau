-- `error(message, level)` prefixes the message with the position of the
-- frame `level` up: level 2 is the caller of the function calling `error`.
-- In a helper's body, that frame moves once the code is a call of the
-- helper: written out in `setSpeed`, level 2 is the caller of `setSpeed`
-- (`pcall`, a C function: no position); in a call `checkNumber(v,
-- "speed")`, it is `setSpeed` (a position). Like `debug.info(2, ...)`, such
-- a call reads the caller's frame, and its helper is never rebuilt.
-- `setSpeed` is a hand-written copy of `checkNumber`'s body, so the source
-- itself has no frame for the helper; `setAngle` calls it, and Luau's -O2
-- copy there drops the helper's frame too (checked against the bytecode,
-- the source keeping the frame: only the message is printed there).
-- Level 1 (or none) names the function calling `error`, in this script on
-- a line no output keeps, and level 0 no position: `need` stays a helper
-- (`need(x, label)`).
-- `_G.error` (`_G["error"]`) is `error`, as `_G.debug` is `debug`: the
-- hand-written copies of `checkName` in `setName` and of the expression
-- helper `needName` in `nameOf` keep their frames too (rebuilt, both
-- printed a position where the source has none).
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

local function where(message)
	return if string.find(message, "^.-:%d+: ") then "with a position" else "without a position"
end

local function text(message)
	return (string.gsub(message, "^.-:%d+: ", ""))
end

checkNumber(4, "count")
print(setSpeed(3), setAngle(8), setLabel(1, "label"), setTitle(2, "title"))
local ok, message = pcall(setSpeed, "fast")
print(ok, text(message), where(message))
ok, message = pcall(setAngle, "wide")
print(ok, text(message))
ok, message = pcall(setLabel, nil, "label")
print(ok, text(message))
checkName("first")
print(setName("ab"), needName("cd"), nameOf("ef"))
ok, message = pcall(setName, 5)
print(ok, text(message), where(message))
ok, message = pcall(nameOf, false)
print(ok, text(message), where(message))
