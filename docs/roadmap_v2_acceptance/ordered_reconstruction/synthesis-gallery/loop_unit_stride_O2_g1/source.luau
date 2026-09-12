local function run(value, flag, sink, tail)
 local total = 0
 for index = 1, 4 do total = total + value * index end
 return total
end
return {run=run}
