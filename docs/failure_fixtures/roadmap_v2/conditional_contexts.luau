return function(flag, emit)
    local argument = if flag then emit("yes", false) else emit("no", nil)
    emit("argument", argument)
    return emit("prefix", 4), if flag then emit("tail", nil) else emit("tail", false)
end
