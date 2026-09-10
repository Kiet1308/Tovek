return function(scope, Children, Label, title, enabled)
    local props = {Name = "Panel", Visible = enabled}
    props[Children] = {
        Label {Text = title},
        scope:New("Frame") {Name = "Body"},
    }
    return scope:New("Frame")(props)
end
