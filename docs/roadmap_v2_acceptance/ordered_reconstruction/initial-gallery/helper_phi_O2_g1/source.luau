local function helper(value, flag)
 local result
 if flag then result = (value * 3 + 5) * 2 else result = (value * 7 + 4) * 3 end
 return result
end
local function run(value, flag, sink, tail)
 local first = helper(value, flag)
 local second = helper(value, false)
 return first, second
end
return {helper=helper, run=run}
