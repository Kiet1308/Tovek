-- V2.7 S2: an update that reuses its variable's register (`offset += 1`,
-- `options = options or {}`) prints as that one variable when the value is
-- read again and the old one is not. A variable a closure captures, or one
-- read after the update, keeps two locals; every print must match.
local trace = {}
local function note(...)
	trace[#trace + 1] = table.concat({ ... }, ",")
end

local function write(buf, offset, a, b)
	note("start", offset)
	offset += 1
	note(offset, a)
	offset += 4
	note(offset, b)
	offset *= 2
	return offset, offset - 1
end
print(write(nil, 3, "x", "y"))

local function defaults(options)
	options = options or {}
	options.size = (options.size or 0) + 1
	return options.size, options.name
end
print(defaults(nil), defaults({ size = 4, name = "n" }))

local function keepsOld(count)
	local next = count + 1
	note(count, next, next * 2)
	return count, next
end
print(keepsOld(10))

local function captured(count)
	local read = function()
		return count
	end
	count += 1
	note(count, read())
	count += 1
	return count, read()
end
print(captured(1))

local function chained(text)
	text = text .. "a"
	note(text, #text)
	text = text .. "b"
	return text, #text
end
print(chained("z"))
print(table.concat(trace, ";"))
