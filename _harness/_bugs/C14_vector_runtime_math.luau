-- Runtime vector math via vector.create / the vector library: no vector
-- constants (so the decompiled side runs fine under plain luau.exe).
local a = vector.create(1, 2, 3)
local b = vector.create(4, -2.5, 0.5)
print(a + b)
print(a - b, a * 2, b / 2, -a)
print(a // 2, b % 1)
print(vector.dot(a, b), vector.cross(a, b), vector.magnitude(a), vector.normalize(b), vector.angle(a, b))
print(vector.normalize(a))
print(vector.floor(b), vector.ceil(b), vector.abs(a), vector.sign(b))
print(vector.clamp(b, vector.zero, vector.create(2, 2, 2)))
print(vector.max(a, b), vector.min(a, b))
local x = a
x += b
x -= b
x *= 2
x //= 2
print(x)
local ok = pcall(vector.create, 0 / 0, 0, 0)
print(ok)
local keyed = {}
keyed[a] = "hit"
print(keyed[a], keyed[vector.create(1, 2, 3)], keyed[b])
print(a == vector.create(1, 2, 3), a == b)