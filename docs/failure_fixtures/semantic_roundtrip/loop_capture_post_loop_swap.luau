-- As loop_capture_post_loop_write, with a swap of two captured locals.
-- Closures created in a loop capture a and b; the final swap is dropped.
local function G(x, y, n)
  local cl = {}
  local a, b = x, y
  for i = 1, n do
    cl[i] = function() return a, b end
  end
  a, b = b, a
  return cl
end
print(G(1, 2, 1)[1]())   -- expected 2 1
