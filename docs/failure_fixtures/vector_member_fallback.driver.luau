for _, mode in {"return", "error"} do
    for _, name in {"magnitude", "normalize", "floor"} do
        local subject = f[name]
        getfenv(subject).vector = {
            [name] = function(...)
                print("replacement", name, select("#", ...), ...)
                if mode == "error" then
                    error("replacement failed", 0)
                end
                return 99, 100
            end,
        }
        print(mode, name, pcall(subject, nil))
    end
end
