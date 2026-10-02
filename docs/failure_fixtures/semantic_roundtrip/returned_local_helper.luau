-- A helper returning a local its closure reads (`local conn; conn =
-- signal:Connect(function() conn:Disconnect() end); return conn`): rebuilt
-- as `local conn = once(...)`, or a bare call when the result is unused, only
-- while nothing writes that local afterwards.
local Signal = {}
Signal.__index = Signal
function Signal.new() return setmetatable({ handlers = {} }, Signal) end
function Signal:Connect(fn)
  local connection = { connected = true }
  function connection:Disconnect() self.connected = false end
  table.insert(self.handlers, function(...) if connection.connected then fn(...) end end)
  return connection
end
function Signal:Fire(...) for _, handler in ipairs(self.handlers) do handler(...) end end

local function once(signal, callback)
  local conn
  conn = signal:Connect(function(...)
    conn:Disconnect()
    callback(...)
  end)
  return conn
end

local log = {}
local signal = Signal.new()
local first = once(signal, function(v) table.insert(log, "a" .. v) end)
local second = once(signal, function(v) table.insert(log, "b" .. v) end)
second = nil
signal:Fire(1)
signal:Fire(2)
print(table.concat(log, ","), first.connected, second)
