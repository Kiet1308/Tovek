local function helper(value) return (value * 3 + 5) * 2 + 11 end
local function alternative(value) return (value * 3 + 5) * 2 + 11 end
local function run(value, flag, sink, tail)
 return helper(value), helper(value)
end
return {helper=helper, alternative=alternative, run=run}
