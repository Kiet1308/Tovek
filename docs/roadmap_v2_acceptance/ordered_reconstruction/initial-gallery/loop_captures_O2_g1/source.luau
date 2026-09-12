local function run(value, flag, sink, tail)
 local callbacks = {}
 for index = 1, 4 do
  local snapshot = index
  callbacks[index] = function() return snapshot end
 end
 return callbacks[1](), callbacks[2](), callbacks[3](), callbacks[4]()
end
return {run=run}
