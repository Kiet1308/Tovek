local count = 0
return {
    Read = function(value) return count, value.Value end,
    Write = function(value, replacement)
        count += 1
        value.Value = replacement
    end,
}
