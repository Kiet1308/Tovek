-- As cell_phi_other_variable, through an if-merge phi: the cell's new
-- version joins its group, so the copy `a = b` is not folded into it.
local function m2(a, b) return b, a end
local function D(v)
  if t == "number" then if v ~= v then return "nan" end return tostring(v) end
  return tostring(v)
end
local function show(...)
  local parts = {}
  for i = 1, select("#", ...) do parts[i] = D((select(i, ...))) end
  print(select("#", ...), table.concat(parts, ","))
end
local function F0(x, y, f1, f2)
  if f2 then
    while n2 < 2 do
    end
    if f2 then
      for it = 1, 2 do
      end
    end
  end
  if b then
    if f1 then
      if b then
      end
      if f1 then
      end
      while n3 < 2 do
      end
    end
  end
  if b then
    for it = 1, 2 do
      if b then
      end
    end
  end
end
local function F1(x, y, f1, f2)
  local trace, cl = {}, {}
  local a, b, c = x, y
  b = b
  if f1 then
  end
  c, a = m2(b, b)
  for it = 1, 2 do
    trace[#trace + 1] = D(c)
  end
  local n1 = 0
  while n1 < 2 do
    n1 = n1 + 1
    if f2 then
      if f2 then
        cl[2] = function() return b end
      end
    end
  end
  cl[3] = function(z) a = z return a end
  for i = 1, 8 do if cl[i] then trace[#trace + 1] = D(cl[i](i)) end end
  return table.concat(trace, ' '), a, b, c
end
show(pcall(F1, 1, 2, true, true))
