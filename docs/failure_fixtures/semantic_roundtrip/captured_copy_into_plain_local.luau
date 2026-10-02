-- `b = a` with `a` captured: the copy carried into `b`'s phi is `b`'s,
-- not a member of `a`'s cell, or `b` becomes `a` and the copy vanishes.
local function m2(a, b) return b, a end
local function D(v)
  local t = type(v)
  return tostring(v)
end
local function show(...)
  local parts = {}
  for i = 1, select("#", ...) do parts[i] = D((select(i, ...))) end
  print(select("#", ...), table.concat(parts, ","))
end
local function F0(x, y, f1, f2)
  local trace, cl = {}, {}
  local a, b = m2(x, y)
  for it = 1, 2 do
    trace[#trace + 1] = D(a)
  end
  local n1 = 0
  while n1 < 2 do
    n1 = n1 + 1
  end
  for it = 1, 2 do
    for it = 1, 2 do
      if b then
        b = (a or 0) + (tonumber(a) or 1)
        b = a
      end
      trace[#trace + 1] = D(a)
    end
    a = b
  end
  cl[1] = function() return a end
  return table.concat(trace, ' '), a, b
end
show(pcall(F0, 1, 2, false, true))
local function F1(x, y, f1, f2)
  for it = 1, 2 do
    if f2 then
      for it = 1, 2 do
      end
    end
  end
end
