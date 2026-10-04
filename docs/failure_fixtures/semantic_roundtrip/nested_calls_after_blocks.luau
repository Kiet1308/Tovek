-- A statement starting with `(` continues the previous line unless a
-- separator ends it: `(f or g)()` after each nested `if` needs none, as
-- `end` closes the `if`. Deciding that must not re-render the nested
-- statements at every level (exponential in the depth).
local calls = 0
local function probe(i) return i >= 0 end
local function run(f, g)
	if probe(0) then
		if probe(1) then
			if probe(2) then
				if probe(3) then
					if probe(4) then
						if probe(5) then
							if probe(6) then
								if probe(7) then
									if probe(8) then
										if probe(9) then
											if probe(10) then
												if probe(11) then
													if probe(12) then
														if probe(13) then
															if probe(14) then
																if probe(15) then
																	if probe(16) then
																		if probe(17) then
																			if probe(18) then
																				if probe(19) then
																					if probe(20) then
																						if probe(21) then
																							(f or g)()
																						end
																						(f or g)()
																					end
																					(f or g)()
																				end
																				(f or g)()
																			end
																			(f or g)()
																		end
																		(f or g)()
																	end
																	(f or g)()
																end
																(f or g)()
															end
															(f or g)()
														end
														(f or g)()
													end
													(f or g)()
												end
												(f or g)()
											end
											(f or g)()
										end
										(f or g)()
									end
									(f or g)()
								end
								(f or g)()
							end
							(f or g)()
						end
						(f or g)()
					end
					(f or g)()
				end
				(f or g)()
			end
			(f or g)()
		end
		(f or g)()
	end
	(f or g)()
end
run(nil, function() calls += 1 end)
print(calls)
