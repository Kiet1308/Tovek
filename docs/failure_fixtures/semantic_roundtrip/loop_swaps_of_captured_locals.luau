-- Swaps of locals captured by closures that only read them: the function
-- itself still writes them, so a read may not move past those writes.
local function D(v)
  return tostring(v)
end
local function show(...)
  local parts = {}
  for i = 1, select("#", ...) do parts[i] = D((select(i, ...))) end
  print(select("#", ...), table.concat(parts, ","))
end
local function F0(x, y, f1, f2)
  local trace, cl = {}, {}
  local a, b, c = x, y
  if a then
    local n2 = 0
    while n2 < 2 do
      n2 = n2 + 1
    end
    if a then
      if c then
      end
    end
    cl[1] = function() return c end
  end
  if a then
  end
  cl[2] = function() return b end
  local n1 = 0
  while n1 < 2 do
    n1 = n1 + 1
    b, c = c, b
    if c then
    end
    for it = 1, 2 do
      a, b = b, a
    end
  end
  return table.concat(trace, ' '), a, b, c
end
show(pcall(F0, 1, 2, true, true))
