-- `a, c = c, a` right after a `for` with a `break`: the swap's copies stay
-- in the join of the loop's exits instead of on both the `break` and the
-- loop's own exit, where no structure can hold them.
local function F(x, y, stop)
  local a, b, c = x, y, x * 10
  if b then
    for it = 1, 2 do
      if stop then break end
    end
    a, c = c, a
  end
  return a, b, c
end
print(F(1, 2, true))
print(F(1, 2, false))
print(F(1, nil, true))
