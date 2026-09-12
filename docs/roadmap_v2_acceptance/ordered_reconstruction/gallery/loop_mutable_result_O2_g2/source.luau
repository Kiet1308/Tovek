local function run(value, flag, sink, tail)
 local total = 0
 local function read() return total end
 for index = 1, 4 do
  total = total + value * index
  sink("observed", read())
 end
 return total
end
return {run=run}
