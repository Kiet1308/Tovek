-- Found by fuzzing (seeds 71089, 71163, gen3 at -O2): `a, b = b, a` where
-- both are captured cells. Destruction joined the copy `D(a)` takes of `a`
-- to `a`'s cell without recording it as an equal ancestor, so the later
-- interference walk, which tests only the closest dominating member and its
-- equal ancestors, missed the old `a` still live at the swap temporary
-- (`tmp = b; b = a; a = tmp`) and printed `a = b; b = a`.
local function m2(a, b) return b, a end
local function m3(a) return a, (a or 0) + 1, nil end
local function D(v)
  local t = type(v)
  return tostring(v)
end
local function show(...)
  local parts = {}
  for i = 1, select("#", ...) do parts[i] = D((select(i, ...))) end
  print(select("#", ...), table.concat(parts, ","))
end
local function F1(x, y, f1, f2)
  local trace, cl = {}, {}
  local a, b = m3(x)
  cl[1] = function() return b end
  if a then
    for it = 1, 2 do
      for it = 1, 2 do
      end
    end
  end
  if f1 then
    trace[#trace + 1] = D(a)
    a, b = b, a
  else
    if b then
      if f1 then
        cl[2] = function() return a end
      end
    end
  end
  for it = 1, 2 do
  end
  return table.concat(trace, ' '), a, b
end
show(pcall(F1, 1, 2, true, true))

local function G0(x, y, f1, f2)
  local trace, cl = {}, {}
  local a, b, c = m2(x, y)
  if f2 then
    if b then
      while n3 < 2 do
        cl[1] = function(z) b = z return b end
      end
    end
  end
  local n1 = 0
  while n1 < 2 do
    n1 = n1 + 1
    if a then
      a, b, a = m3(b)
    end
  end
  for it = 1, 2 do
  end
  if a then
    cl[2] = function() return a end
    while n2 < 2 do
    end
  else
    trace[#trace + 1] = D(a)
    a, b = b, a
  end
  return table.concat(trace, ' '), a, b, c
end
show(pcall(G0, 1, 2, true, nil))
