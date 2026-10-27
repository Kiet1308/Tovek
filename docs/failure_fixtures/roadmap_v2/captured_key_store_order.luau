return function(key, replacement, flag, ...)
    local function change()
        key = replacement
        print("change")
        return 9, "discarded"
    end
    local function stamp()
        print("stamp")
        return nil
    end
    return {
        [key] = change(),
        later = stamp(),
        if flag then stamp() else change(),
        ...,
    }
end
