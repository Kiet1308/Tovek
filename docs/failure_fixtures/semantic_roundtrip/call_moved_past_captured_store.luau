-- `local before = getCount(); count = 0; log(before)`: the call may not move
-- past the store into the cell its closure reads.
local count = 5
local function getCount()
  return count
end
local function log(...)
  print(...)
end
local function reset()
  local before = getCount()
  count = 0
  log("reset from", before)
end
reset()
