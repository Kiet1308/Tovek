-- A global another chunk sets (`G`) is read after the helper's leading call
-- that sets it, so the read may not move ahead of the rebuilt call.
loadstring("function setG(v) G = v end")()
local function h(v)
	setG(v)
	if v then
		return "set"
	end
	return "clear"
end
for _, x in ipairs({ 5, false }) do
	local r = h(x)
	print(G, r)
end
