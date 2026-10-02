-- `local frames = result` copies a captured local no closure writes, then
-- is reassigned: joined to the cell, `frames = {...}` would overwrite
-- `result` for the later iterations and the closure.
local function build(folders)
  local result, fps = nil, nil
  if folders.enabled then
    result = {"base"}
    fps = 10
  end
  local entries = {}
  for _, child in ipairs(folders) do
    local frames = result
    local rate = fps
    if child.flip then
      frames = {child.name}
      rate = child.rate
    end
    entries[#entries + 1] = {frames = frames, fps = rate or 30}
  end
  local function current() return result and result[1], fps end
  return entries, current
end
local entries, current = build({enabled = true, {name = "a", flip = true, rate = 5}, {name = "b"}})
for _, e in ipairs(entries) do print(e.frames and e.frames[1], e.fps) end
print(current())
