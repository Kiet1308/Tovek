return function(scope, Children, Label, title, enabled)
    local props = {Name = "Panel"}
    if enabled then
        props.BackgroundTransparency = 0
    else
        props.BackgroundTransparency = 1
    end
    props[Children] = {Label {Text = title}}
    return scope:New("Frame")(props)
end
