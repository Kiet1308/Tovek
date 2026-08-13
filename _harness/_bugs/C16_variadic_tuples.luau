-- Variadic tuples: forwarding `...`, select slicing/counting, tuple truncation
-- and expansion in assignments, tuple-through-table, nested forwarding, tailcall
-- recursion returning multiple values.
local function pack(...) return ... end
local function sandwich(a, ...) return a + select(1, ...) + select("#", ...), ... end
local function slice3(...) return select(3, ...) end
local x1, x2 = pack(1, 2, 3, 4)
print(x1, x2)
local r, tail1, tail2 = sandwich(10, 1, 2, 3)
print(r, tail1, tail2, select("#", sandwich(10, 1, 2, 3)))
print(select("#", pack("a", "b", "c")), select(2, pack(1, 2, 3)))
local a, b, c = slice3(9, 8, 7, 6, 5)
print(a, b, c)
local function zero() return end
local function mixed() return 1, nil, 3 end
print(select("#", mixed()), mixed())
local function chain(n)
	if n <= 0 then return "done", n end
	return chain(n - 1)
end
print(chain(4))
local t = { pack(1, 2, 3) }
print(t[1], t[2], t[3], #t)
local function deep(...) return pack(...) end
local u, v, w, q = deep(5, 6, 7, 8)
print(u, v, w, q, select("#", deep(5, 6, 7, 8)))
local function tailRet(...) return ... end
print(tailRet(1, 2, 3), select("#", tailRet(1, 2, 3)))
print(tailRet())