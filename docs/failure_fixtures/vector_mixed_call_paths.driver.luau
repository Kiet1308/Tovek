Vector3 = {
    new = function(...)
        print("replacement", ...)
        return 99, 100
    end,
}
print("true", f(true, 2))
print("false", f(false, 2))
