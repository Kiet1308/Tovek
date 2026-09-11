local Measures = require(script.Parent.Measures)
return {
 measures = Measures,
 run = function(record)
  return Measures(record)
 end,
}
