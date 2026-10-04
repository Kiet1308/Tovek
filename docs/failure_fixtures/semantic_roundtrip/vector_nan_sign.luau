-- A vector constant keeps each component's bits: a negative NaN stays
-- negative, which no number spelling carries.
local function f()
	return vector.create(0 / 0, 1, 2), vector.create(-(0 / 0), 1, 2)
end
local a, b = f()
local buf = buffer.create(4)
buffer.writef32(buf, 0, a.x)
print(buffer.readu32(buf, 0))
buffer.writef32(buf, 0, b.x)
print(buffer.readu32(buf, 0))
