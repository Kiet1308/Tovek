-- A builtin call whose last argument gives all its results is no FASTCALL2:
-- Luau copies `list` into place before `obj:Grab()` runs, so the insert
-- lands in the table `list` held before the call wrote it, and the copy may
-- fold back into `table.insert(list, obj:Grab())`.
local function insert_before_call()
  local list = {}
  local first = list
  local obj = {}
  function obj:Grab() list = {} return "item" end
  table.insert(list, obj:Grab())
  return #first, #list
end
print(insert_before_call())
