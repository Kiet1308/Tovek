return function(name)
 local other = require(script.Parent[name])
 return other()
end
