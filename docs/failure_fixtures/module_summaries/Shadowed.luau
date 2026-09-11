local require = function(_) return custom end
local other = require(script.Parent.Measures)
return function(value) return other(value) end
