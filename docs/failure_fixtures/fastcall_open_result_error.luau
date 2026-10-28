return {
    method = function(out, count)
        table.insert(out, ("x"):rep(count))
    end,
    open = function(out, multi)
        table.insert(out, multi())
    end,
}
