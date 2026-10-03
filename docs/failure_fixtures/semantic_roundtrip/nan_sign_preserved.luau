-- NaN constants keep their sign: `0 / 0` folds to the NaN with the sign bit
-- set (x86) and `-(0 / 0)` to the other, which `buffer.writef64` shows.
local b = buffer.create(8)
buffer.writef64(b, 0, -(0 / 0))
print(buffer.readu32(b, 4))
buffer.writef64(b, 0, 0 / 0)
print(buffer.readu32(b, 4))
local x = 2
buffer.writef64(b, 0, x ^ -(0/0))
print(buffer.readu32(b, 4))
buffer.writef64(b, 0, -(-(0/0)))
print(buffer.readu32(b, 4))
print(vector.create(-(0/0), 0/0, 1).x ~= 0)
