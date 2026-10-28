for _, mode in {"return", "error"} do
    for _, name in {
        "direct", "alias", "named", "captured", "capturedNegation", "capturedAddition",
        "namedAddition", "namedTuple", "constants", "effectful", "parameters",
    } do
        local subject = f[name]
        getfenv(subject).math = setmetatable({}, {__index = function(_, key)
            print("lookup", name, key)
            return function(...)
                print("call", select("#", ...), ...)
                if mode == "error" then
                    error("replacement failed", 0)
                end
                return 99, 100
            end
        end})
        print(mode, name, pcall(subject, 1, 2))
    end
end
