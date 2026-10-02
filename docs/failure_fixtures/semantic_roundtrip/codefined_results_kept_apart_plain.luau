-- As codefined_results_kept_apart, without closures (`local a, b, c = m3(x)`).
local function m2(a, b) return b, a end
local function m3(a) return a, (a or 0) + 1, nil end
local function D(v)
  return tostring(v)
end
local function show(...)
  local parts = {}
  for i = 1, select("#", ...) do parts[i] = D((select(i, ...))) end
  print(select("#", ...), table.concat(parts, ","))
end
local function F0(x, y, f1, f2)
  if a then
    if a then
      while n3 < 2 do
      end
    end
  end
  while n1 < 2 do
    for it = 1, 2 do
      if f2 then
      end
    end
  end
  if f1 then
    for it = 1, 2 do
    end
  end
  if f1 then
  end
end
local function F1(x, y, f1, f2)
  local trace, cl = {}, {}
  local a, b, c = m3(x)
  if f2 then
  else
    c, a = a, c
    for it = 1, 2 do
      if c then
      end
    end
  end
  c, c = m2(0, a)
  if a then
    for it = 1, 2 do
      if b then
      end
    end
  end
  return table.concat(trace, ' '), a, b, c
end
show(pcall(F1, 1, 2, true, true))
local function F2(x, y, f1, f2)
  if a then
    while n2 < 2 do
    end
  end
end
