-- A numeric `for` preparation runs no code, so the init edge's copy out of
-- a captured cell belongs before it, not in a suffix no `for` can express.
local cl, trace = {}, {}
local function m2(a, b) return b, a end
local function m3(x) return x, x end
local function F0(x, f1)
  local a, b = m3(x)
  b = a
  b = (b or 0) + (tonumber(b) or 1)
  if b then
    b, a = m2(nil, a)
  end
  if f1 then
    cl[1] = function(z) a = z return a end
  else
    for it = 1, 2 do
      for it = 1, 2 do
        if cl[1] then trace[#trace + 1] = tostring(cl[1](a)) end
      end
      if a then
      end
    end
    for it = 1, 2 do
    end
  end
  return a, b
end
print(F0(3, true))
print(F0(4, false))
print(cl[1](7), table.concat(trace, ","))
