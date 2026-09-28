local calls = 0
local total, larger, item, label, later = f(function(bump)
    calls += 1
    bump()
    return calls
end)
print(total, larger, item, label, later(), calls)
