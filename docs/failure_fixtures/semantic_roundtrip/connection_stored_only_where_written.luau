-- A connection is stored into the cell its callback disconnects only where
-- the source stores it: a callback firing while its cell is still nil must
-- not disconnect.
local log = {}
local signal = {}
function signal:Connect(callback)
  self.callback = callback
  return { Disconnect = function() table.insert(log, "disconnected") end }
end
local function stored()
  local connection
  connection = signal:Connect(function()
    if connection then connection:Disconnect() end
    table.insert(log, "fired")
  end)
  signal.callback()
end
local function unstored()
  local connection
  local unused = signal:Connect(function()
    if connection then connection:Disconnect() end
    table.insert(log, "early")
  end)
  signal.callback()
  connection = nil
end
stored()
unstored()
print(table.concat(log, ","))
