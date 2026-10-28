-- Make actual native calls use their replaceable fallback.
getfenv()
math = table.clone(math)
math.abs = function(value)
    return "replacement-abs", value
end
