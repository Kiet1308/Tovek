return function(mode, condition, primary, fallback, observe)
    if mode == "select" then
        local selected = if condition then primary else fallback
        observe(selected)
        return selected
    elseif mode == "normalize" then
        if primary == nil then
            primary = #fallback
        end
        observe(primary)
        return primary
    else
        local function change()
            primary = fallback
        end
        local function read()
            return primary
        end
        if condition then
            change()
        end
        observe(read())
        return primary
    end
end
