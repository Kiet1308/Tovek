-- At -O2 a helper whose `return` leaves a `while` from inside is inlined: the
-- `return` jumps past the code after the loop, which the loop's own exits
-- (its test, a `break`) run. That code may branch, so no `break` can carry a
-- copy of it: it runs after the loop under a flag the `return` clears.
-- Every such script failed whole ("residual goto/label would be invalid Luau").
local function report(p0, p1)
  local w = 0
  while w < 3 do
    w = w + 1
    if w > p0 then return end
    if w == 2 then break end
  end
  if p1 > 2 then print("big") end
end
for i = 1, 3 do
  if i == 2 then continue end
  report(i, i)
  print(i)
end
report(1, 3)
report(5, 3)
local function score(p0, p1)
  local w = 0
  while w < 3 do
    w = w + 1
    if w > p0 then return w end
    if w == 2 then break end
  end
  if p1 > 2 then return -1 end
  return 0
end
local total = 0
for i = 1, 3 do
  total = total + score(i, i)
  print(i, total)
end
local function count_up(p0, p1)
  local w = 0
  while w < 3 do
    w = w + 1
    if w > p0 then return end
    if w == 2 then break end
  end
  for j = 1, p1 do print("tail", j) end
end
for i = 1, 3 do
  count_up(i, i)
end
