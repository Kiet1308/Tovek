-- `t.c = g(); t.a = h()` on a template `{a = 1, b = 2}`: `h()` may take the
-- place of `a` only ahead of inert entries, never ahead of the appended `g()`.
local log = {}
function k() table.insert(log, "k") return 10 end
function g() table.insert(log, "g") return 1 end
function h() table.insert(log, "h") return 2 end
local t = {a = 1, b = 2}
local n = k()
t.c = g()
t.a = h()
print(table.concat(log, ","), n, n + 1, t.a, t.b, t.c)
