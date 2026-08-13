-- String interpolation: format specs, nested backtick strings, escaped braces
-- and backticks, unicode escapes, expression rendering, long brackets.
local name = "Tovek"
local x = 3.14159
print(`hi {name} {x:.2f}`)
print(`a {`b {x} c`} d`)
print(`{10 > 5 and "yes" or "no"}`)
print(`{x * 1e6}`)
print(`\{brace\}`)
print(`a \` b`)
print(`\u{0041} \u{42}`)
local raw = "tab\there"
print(`[{raw}]`)
print("line1\nline2\t\"q\" \\ end")
local multi = [[
	keep
	indent
]]
print(multi)
local acc = ""
for i = 1, 5 do
	acc ..= `[{i}]` .. "{{"
end
print(acc)
print(string.format("Tovek-%s-%d-%.1f", name, 42, 2.25))