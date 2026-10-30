-- A script's own `local createVector = vector.create` is the binding its vector
-- constants print through, not a second `createVector_2`; a zero-vector sum is
-- `total`, its mean `average`.
local createVector = vector.create
local points = {createVector(1,2,3), createVector(4,5,6)}
local sum = createVector(0, 0, 0)
for _, p in points do
	sum += p
end
local avg = sum / #points
print(avg)
