local function run(value, flag, sink, tail)
 for index = 1, 4 do sink("step", value * index) end
 return value
end
return {run=run}
