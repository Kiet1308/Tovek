type = function(...)
    print("host-type", select("#", ...))
    return "replacement-type", 101
end
typeof = function(...)
    print("host-typeof", select("#", ...))
    return "replacement-typeof", 102
end
