return {
    varargs = function(out, ...)
        table.insert(out, ...)
    end,
    ordinary = function(callee, out, multi)
        callee(out, multi())
    end,
}
