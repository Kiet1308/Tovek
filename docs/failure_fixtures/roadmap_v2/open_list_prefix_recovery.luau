local function mark(label, value)
    print(label)
    return value, "discarded"
end

return {
    keyed = function(key, flag, ...)
        return {
            [key] = mark("value", 1),
            second = mark("second", 2),
            if flag then mark("yes", 3) else mark("no", 4),
            ...,
        }
    end,
    positional = function(key, flag, ...)
        return {
            mark("array", 10),
            [key] = mark("value", 1),
            second = mark("second", 2),
            if flag then mark("yes", 3) else mark("no", 4),
            ...,
        }
    end,
}
