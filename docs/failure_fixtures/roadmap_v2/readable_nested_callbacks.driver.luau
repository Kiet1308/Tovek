for _, flag in {false, true} do
    print(f.outer.callback({CharacterTransparencyOverrides = flag}), f.outer.short(flag))
end
