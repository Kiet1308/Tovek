local function helper(value)
 local product = value * 3
 return (product + 5) * 2 + 11
end
local function run(value, flag, sink, tail)
 local first = helper(value)
 local second = helper(value)
 return first, second
end
return {helper=helper, run=run}
