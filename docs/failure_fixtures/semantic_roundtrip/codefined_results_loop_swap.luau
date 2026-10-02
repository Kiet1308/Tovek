-- A swap of a captured local in a loop with a conditional break; merging
-- values one statement defines left the destructor without a definition.
local function D(v)
end
local function show(...)
end
local function F0(x, y, f1, f2)
  local a, b, c = m2(x, y)
  while n1 < 2 do
    cl[1] = function(z) c = z return c end
  end
  if b then
    if f1 then
    end
    for it = 1, 2 do
      c, b = b, c
      if f1 then break end
    end
    trace[#trace + 1] = D(b)
  end
  b = c
  trace[#trace + 1] = D(b)
end
