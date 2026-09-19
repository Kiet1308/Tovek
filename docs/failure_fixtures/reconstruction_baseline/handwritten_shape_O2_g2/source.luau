local function helper(value)
 return (value * 3 + 5) * 2 + 11
end
local function run(value, flag, sink, tail)
 local first = (value * 3 + 5) * 2 + 11
 local second = (value * 3 + 5) * 2 + 11
 return first, second
end
return {helper=helper, run=run}
