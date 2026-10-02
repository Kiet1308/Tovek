-- `a, c = c, a` in a loop, `c` captured after it: `a`'s inner loop phi copies
-- `c`'s cell value but stays `a`'s; replaced by the cell, the swap moved to
-- both exits of the inner loop and the loop no longer structured.
local cl = {}
local function m3(x) return x, 2, x * 10 end
local function F1(x)
  local a, b, c = m3(x)
  local trace = {}
  for it = 1, 2 do
    a, c = c, a
    for it2 = 1, 2 do
      trace[#trace + 1] = tostring(a)
      if a then break end
    end
  end
  local n = 0
  while n < 2 do
    n += 1
    cl[1] = function(z) c = z return c end
  end
  return table.concat(trace, " "), a, b, c, cl[1](7), c
end
print(F1(1))
