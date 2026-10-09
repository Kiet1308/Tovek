-- Two bindings of one scope that role evidence names alike (both stored
-- under `.size`) keep distinct names. The rejected scope-entry clock gave a
-- binding declared after its scope's last child an empty region, so
-- `local a = 1; local b = 2; return a + b` could read
-- `local size = 1; local size = 2; return size + size`.
local T = {}

local function measure(a, b)
	local first = a * 2
	local second = b * 3
	T.size = first
	T.size = second
	return first + second
end

-- A name used only inside an earlier block is free again after it.
local function sequence(flag)
	if flag then
		local part = { flag }
		T.part = part
		print(#part)
	end
	local part = { 1, 2, 3 }
	T.part = part
	return #part
end

print(measure(1, 2), T.size)
print(sequence(true), sequence(false), #T.part)
