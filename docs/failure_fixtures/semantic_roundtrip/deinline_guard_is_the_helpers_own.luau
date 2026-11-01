-- Copies Luau -O2 made of `assertSignalLike(signal)`: the helper's `if`
-- tests its argument's type and errors. A constant inferred from that
-- branch's truth (`value` nil makes `typeof(value) ~= "table"` hold) once
-- rebuilt the copy's arm alone, leaving the `if` around it:
-- `if typeof(value) ~= ... then assertSignalLike() end`. The `if` runs on
-- the actual argument: it is the helper's own, and the copy is the whole
-- `if`, rebuilt as `assertSignalLike(signal)`.
local function assertSignalLike(value)
	if typeof(value) ~= "RBXScriptSignal" and (typeof(value) ~= "table" or typeof(value.Connect) ~= "function") then
		error("did not receive a signal as an argument", 3)
	end
end
local Trove = {}
Trove.__index = Trove
function Trove:Connect(signal, fn)
	assertSignalLike(signal)
	return signal:Connect(fn)
end
function Trove:Once(signal, fn)
	assertSignalLike(signal)
	return signal:Once(fn)
end

local fake = {}
function fake:Connect(fn) return "connected " .. fn() end
function fake:Once(fn) return "once " .. fn() end
local trove = setmetatable({}, Trove)
print(trove:Connect(fake, function() return "a" end), trove:Once(fake, function() return "b" end))
local ok, problem = pcall(function() return trove:Connect(5, print) end)
print(ok, (string.gsub(tostring(problem), "^.-:%d+: ", "")))
