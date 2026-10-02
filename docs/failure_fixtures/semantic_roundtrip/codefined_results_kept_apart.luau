-- Two results of one call (`b, a = m2(a, b)`) are alive together from the
-- call: out-of-SSA coalescing must never give them one variable.
local function m2(a, b) return b, a end
local function F0(x, y)
  local a, b = x, y
  b, a = m2(a, b)
  if a then
    b, a = a, b
  end
  local get = function() return a end
  return a, b, get()
end
print(F0(1, 2))
