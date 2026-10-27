return {
    outer = {
        callback = function(options)
            if options.CharacterTransparencyOverrides then
                return false
            else
                return nil
            end
        end,
        short = function(flag)
            if flag then return false else return nil end
        end,
    },
}
