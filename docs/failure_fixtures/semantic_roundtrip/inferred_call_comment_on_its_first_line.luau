-- A rebuilt call that goes on past its first line keeps its comment on that
-- line. `trackConn(player.UserId, signal:Connect(function(who) ... end))`
-- printed `-- inferred equivalent call` after the closing `end))`, lines
-- below the call and right under the closure's own last line, where it read
-- as a comment on the closure. Now the comment ends the line the call
-- starts on: after `function(who)`, or after the `{` of a hugged table.
-- Luau -O2 inlines `trackConn` at all three calls in `setup` (the helper
-- stays a local because it is also stored twice as a value), and Tovek
-- rebuilds them. The hand-written lookalike at the end of `setup` is no
-- copy of the helper (a call sits between its two statements) and stays as
-- written.
local tracked = {}

local function trackConn(userId, connection)
	tracked[userId] = tracked[userId] or {}
	table.insert(tracked[userId], connection)
end

local exported = { track = trackConn, again = trackConn }

local signal = {
	Connect = function(self, callback)
		callback("fired")
		return { Connected = true, callback = callback }
	end,
}

local function setup(player)
	trackConn(player.UserId, signal:Connect(function(who)
		print("triggered", who)
		print("again", who)
	end))
	trackConn(player.UserId, signal:Connect(function()
		print("removing", player.UserId)
	end))
	trackConn(player.UserId, {
		Connected = player.UserId > 2,
		Name = "plain",
		Extra = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10 },
		Deep = { a = 1, b = { c = 2 } },
	})
	tracked[0] = tracked[0] or {}
	print("between")
	table.insert(tracked[0], "manual")
end

setup({ UserId = 7 })
setup({ UserId = 1 })
exported.again(1, "direct")
for id, list in tracked do
	print(id, #list, type(list[1]))
end
return setup
