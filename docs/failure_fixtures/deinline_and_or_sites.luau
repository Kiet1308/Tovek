-- `a and f(x)` and `a or f(x)` with `f` inlined: the call rebuilt in the
-- branch folds back into the operator (`local v = a; if v then v = f(x) end`).
local t = {1, 2, 3}
local function pick(list, i)
	if not list then
		return nil
	end
	local v = list[i]
	if v == nil then
		return 0
	end
	return v * 2 + #list
end
print(t[1] and pick(t, 2), nil, 2)
print(t[1] or pick(t, 3))
local x = t[5] and pick(t, 1)
print(x)
