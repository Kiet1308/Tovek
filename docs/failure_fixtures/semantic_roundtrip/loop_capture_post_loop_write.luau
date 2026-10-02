-- Closures made in a loop capture `enabled`; the write after the loop must
-- reach them. Replacing the cell's loop phi by the value before the loop
-- split the cell, and the write went to a variable nobody read.
-- A local captured by closures created inside a loop, then assigned after the loop.
-- The post-loop write is dropped (or lands in a different variable).
local function a3(t)
  local enabled = t.flag
  for _, item in ipairs(t) do item.f = function() return enabled end end
  enabled = true
  return t
end
print("field", a3({{}, flag = false})[1].f())   -- expected true
local function v4(n)
  local fs = {}
  for i = 1, 2 do fs[i] = function() return n end end
  n = n * 10
  return fs
end
print("param", v4(3)[1]())                      -- expected 30
local function k(x)
  local b = x
  local fs = {}
  for i = 1, 2 do fs[i] = function(z) b = z return b end end
  b = x * 2
  return b, fs[1](5), b
end
print(k(3))                                     -- expected 6 5 5
