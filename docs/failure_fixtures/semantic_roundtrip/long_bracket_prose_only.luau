-- Any string with two newlines printed as a long bracket, so Lua patterns
-- and messages framed by newlines read as `[[\n^.-\n(.-)\n]]` and
-- `[[\nPromise created at:\n\n]]`. Long brackets now hold prose only: three
-- lines or more, two of them at least 8 characters, no newline at either
-- end, nothing that marks a pattern or format string (a leading `^`, `%`
-- before a letter, `(.-)`). Everything else is quoted with escapes; both
-- spellings are the same bytes.
local prose = "Something went wrong while saving your data.\nPlease try again in a few minutes,\nor contact support."
local framed = "\nPromise created at:\n\n"
local pattern = "^.-\n(.-)\n"
local format = "Expected %s\nbut received %s\nat index %d"
local short = "a\nb\nc"

local traceback = "first line\nsecond line\nthird line"
print(string.match(traceback, pattern))
print(#prose, prose)
print(#framed, framed)
print(format:format("number", "string", 3))
print(#short, short)
