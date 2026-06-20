local ok = pcall(function()
	local a, b = {}, {}
	if a < b then end
	return "noerror"
end)
print(ok)
