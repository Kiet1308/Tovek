local calls = 0
getfenv(f).math = {
    abs = function(value)
        calls += 1
        print("replacement", value)
        return 100 + value
    end,
}
f()
print("calls", calls)
