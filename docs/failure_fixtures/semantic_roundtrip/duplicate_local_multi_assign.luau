-- A multiple assignment must not declare one local twice for two values.
local function pick(s, flag)
	local a, b = string.match(s, "(%a)(%a)")
	if flag then
		b = a
	end
	return b
end
print(pick("xy", false), pick("xy", true))
