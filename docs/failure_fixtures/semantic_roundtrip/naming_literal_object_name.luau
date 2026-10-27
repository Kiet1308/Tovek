local function build(flag, text)
    local first = {}
    first.Name = "Greeting"
    first.Value = 12
    local second = {}
    if flag then second.Name = "Head" else second.Name = "Tail" end
    local third = {}
    third.Name = "Initial"
    third.Name = text
    local fourth = {}
    fourth.Name = "Initial"
    local ignored
    ignored, fourth.Name = string.find(text, "a")
    print(first.Name, first.Value, second.Name, third.Name, ignored, fourth.Name)
    return first, second
end
for _, flag in { false, true } do
    local a, b = build(flag, "day")
    print(a.Value, b.Name)
end
