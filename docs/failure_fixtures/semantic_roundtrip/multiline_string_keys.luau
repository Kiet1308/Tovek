-- A key printed as a long string must not touch the index bracket:
-- `t[[[...]]]` would read as a long string from the first `[[`.
local function f(t)
	t["hello\nworld\n"] = 42
	return t["hello\nworld\n"], ({ ["hello\nworld\n"] = 7 })["hello\nworld\n"]
end
print(f({}))
