local function run(value, flag, sink, tail)
 local last = false
 for index = 1, 4 do last = index; sink("index", last) end
 return last
end
return {run=run}
