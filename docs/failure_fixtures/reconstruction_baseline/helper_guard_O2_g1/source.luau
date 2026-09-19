local function helper(value, flag)
 if flag then return (value * 3 + 5) * 2 end
 return (value * 7 + 4) * 3
end
local function run(value, flag, sink, tail)
 local first = helper(value, flag)
 local second = helper(value, false)
 return first, second
end
return {helper=helper, run=run}
