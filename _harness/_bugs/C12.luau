local function run()
  local trace = {}
  for i = 1, 3 do
    for j = 1, 3 do
      for k = 1, 3 do
        if k == 2 and j == 2 then
          trace[#trace + 1] = `break-k @ {i},{j},{k}`
          break
        end
        if j == 3 and k == 1 then
          trace[#trace + 1] = `break-j @ {i},{j},{k}`
          break
        end
        trace[#trace + 1] = `{i}.{j}.{k}`
      end
      if i == 2 and j == 2 then
        trace[#trace + 1] = `break-outer-j @ {i},{j}`
        break
      end
    end
  end
  return trace
end
local t = run()
for _, s in ipairs(t) do print(s) end
print("count", #t)