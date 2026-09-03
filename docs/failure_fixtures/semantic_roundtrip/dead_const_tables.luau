-- ROADMAP C5: a dead constant table (config / enum / closure table the source
-- never reads) must survive as `local _ = { ... }` — its strings, numbers and
-- function bodies are information — and `local t = {}; t.k = v` must never
-- collapse into `({}).k = v`.
local ASSETS = { ShieldImage = "rbxassetid://134972281307212", Width = 520 }
local NAMES = { "IchigoPassive1", "IchigoPassive2", [5] = "NoellePassive" }
local Handlers = {}
Handlers.Formatter = function(x)
	return x .. "!"
end
local unusedEmpty = {}
local M = {}
function M.run(a)
	local scratch = { a, a + 1 }
	return a + 1
end
print(M.run(41))
