return function(condition, primary, fallback)
    local selected = if condition then primary else fallback
    return selected, if selected == nil then "missing" else "present"
end
