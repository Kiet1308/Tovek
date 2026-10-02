-- A copy out of a captured local merges with it only when no closure, not
-- even one a closure creates (UPVAL), writes it and it is not written
-- while the copy is live.
local function deep(t)
  local x = t.a
  if t.c then x = t.b end
  local outer = function()
    return function(v) x = v end
  end
  local snapshot = x
  outer()(9)
  print(snapshot, x)
end
deep({a = 1, b = 2, c = true})
local function plain(t)
  local x = t.a
  if t.c then x = t.b end
  local read = function() return x end
  local snapshot = x
  print(snapshot, read())
  x = 5
  print(snapshot, read(), x)
end
plain({a = 1, b = 2, c = true})
local function later(t)
  local x = t.a
  if t.c then x = t.b end
  local snapshot = x
  local f = function() return x end
  x = x + 1
  print(snapshot, f(), x)
end
later({a = 1, b = 2, c = true})
