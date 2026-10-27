for _, flag in {false, true} do
    local value = f.nan(flag)
    local bytes = buffer.create(16)
    buffer.writef64(bytes, 0, value)
    buffer.writef64(bytes, 8, if flag then 0 / 0 else -(0 / 0))
    print(flag, f.negative(flag), f.index(flag, {[-1] = 3, [-2] = 4}),
        f.infinity(flag), value ~= value, buffer.readu32(bytes, 4) == buffer.readu32(bytes, 12), 1 / f.zero(flag))
end
