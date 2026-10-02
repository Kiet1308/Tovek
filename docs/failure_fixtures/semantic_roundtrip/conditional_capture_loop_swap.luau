-- A swap in a loop of a conditionally captured local: the cell's loop phi
-- may only stand for a version defined before the loop, not for `b = a`
-- written in its body, or the swap loses its temporary.
-- A conditionally created (unused) closure captures b; a swap in a loop is then
-- lowered as two sequential copies without a temporary.
local function f(flag, x, y)
  local a, b = x, y
  local g
  if flag then g = function() return b end end
  for i = 1, 3 do
    a, b = b, a
  end
  return a, b
end
print(f(false, 1, 2))   -- expected 2 1
