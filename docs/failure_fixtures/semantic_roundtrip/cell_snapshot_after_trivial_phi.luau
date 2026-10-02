-- A trivial phi of a captured local replaced by its earlier version: that
-- version is the cell now, so `local snapshot = x` stays a copy and does
-- not merge with `x`, which a closure writes before the print.
local function f(t, c)
  local x = t.a
  if c then x = t.b end
  if t.z then print("z") end
  local set = function(v) x = v end
  local snapshot = x
  set(5)
  print(snapshot, x)
  return snapshot
end
f({a = 1, b = 2}, true)
local function g(t)
  local x = t.a
  if t.c then x = t.b end
  for i = 1, 2 do
    t[i] = function(v) x = v end
  end
  local snapshot = x
  t[1](7)
  print(snapshot, x)
end
g({a = 1, b = 2, c = true})
