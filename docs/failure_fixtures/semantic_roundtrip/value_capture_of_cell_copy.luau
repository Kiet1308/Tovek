-- `local frozen = value` captured by value (never reassigned) next to the
-- captured `value`: joined to `value`'s cell, the closure would see the
-- later writes, as every capture in source is by reference.
local function f(input)
  local value = input
  local function read() return value end
  value = value - 4
  local frozen = value
  local function getter() return frozen, value end
  value = value + 10
  local first, second = getter()
  return first, second, read()
end
print(f(5))
