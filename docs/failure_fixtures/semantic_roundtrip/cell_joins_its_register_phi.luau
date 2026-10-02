-- A captured local's value carried into its own register's loop phi: the
-- phi joins the cell's class instead of the transport sitting in two
-- classes, which made out-of-SSA refuse the merge (a structuring failure).
local cl, trace = {}, {}
local n = 0
local function m3(v)
  n = n + 1
  return n % 2 == 0 and n or nil, (v or 0) + 1, n
end
local function F0(x, f1, f2)
  local a, b, c, d = m3(x)
  d = 1
  if d then
    if a then
      for it = 1, 2 do
        cl[1] = function(z) c = z return c end
      end
    else
      for it = 1, 2 do
        c, d, a = m3(d)
        cl[4] = function(z) b = z return b end
        if c then break end
      end
      if f1 then
        cl[5] = function() return c end
      end
    end
    if f1 and a then
      cl[9] = function() return c end
    end
  end
  return a, b, c, d
end
print(F0(1, true, true))
for i = 1, 9 do if cl[i] then print(i, cl[i](i * 10)) end end
