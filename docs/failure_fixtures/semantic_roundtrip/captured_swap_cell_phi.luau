-- A captured local swapped in a loop: its loop phi stands for the version
-- flowing in from before the loop, never one the loop body writes.
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
  local trace, cl = {}, {}
  local a, b, c = x, y
  if b then
    local n2 = 0
    while n2 < 2 do
      n2 = n2 + 1
    end
    if a then
      for it = 1, 2 do
        cl[1] = function(z) a = z return a end
        trace[#trace + 1] = D(c)
        c, a = a, c
      end
    end
  else
    trace[#trace + 1] = D(c)
    if b then
      if b then
        cl[2] = function(z) a = z return a end
      end
    end
  end
  return table.concat(trace, ' '), a, b, c
end
show(pcall(F0, 1, 2, true, true))
