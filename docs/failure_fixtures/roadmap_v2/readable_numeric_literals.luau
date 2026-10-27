return {
    negative = function(flag)
        if flag then return -1 else return 2 end
    end,
    index = function(flag, values)
        if flag then return values[-1] else return values[-2] end
    end,
    infinity = function(flag)
        if flag then return 1 / 0 else return -1 / 0 end
    end,
    nan = function(flag)
        if flag then return 0 / 0 else return -(0 / 0) end
    end,
    zero = function(flag)
        if flag then return 0 else return -0 end
    end,
}
