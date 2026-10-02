-- An unused local is not named `_` where the script reads the global `_`.
_ = "G"
local t = {10, 20}
for k, v in pairs(t) do
	print(v, _)
end
local function f() return 5 end
local x = f()
print(_)
