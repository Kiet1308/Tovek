local function choose(condition, first, second)
    local result
    if condition then
        result = first
    else
        result = second
    end
    print(result, first, second)
    return {SelectedCharacterTransparencyOverrideValue = result}
end

print(choose(true, false, 7).SelectedCharacterTransparencyOverrideValue)
print(choose(false, false, 7).SelectedCharacterTransparencyOverrideValue)
print(choose(true, nil, 7).SelectedCharacterTransparencyOverrideValue)
return choose
