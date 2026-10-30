-- A virtual machine dispatching on a binary tree of `if`s: every arm updates
-- the loop-carried `acc`, one phi web the copy coalescer grew one arm at a
-- time with a walk over the whole web per arm (quadratic).
local stack = {}
for i = 1, 50 do
	stack[i] = i
end
local function run(code)
	local acc, pc = 0, 1
	while pc <= #code do
		local op = code[pc]
		if op < 128 then
			if op < 64 then
				if op < 32 then
					if op < 16 then
						if op < 8 then
							if op < 4 then
								if op < 2 then
									if op < 1 then
										acc = acc + stack[1] * 0; stack[1] = acc % 1000
									else
										acc = acc + stack[2] * 1; stack[2] = acc % 1000
									end
								else
									if op < 3 then
										acc = acc + stack[3] * 2; stack[3] = acc % 1000
									else
										acc = acc + stack[4] * 3; stack[4] = acc % 1000
									end
								end
							else
								if op < 6 then
									if op < 5 then
										acc = acc + stack[5] * 4; stack[5] = acc % 1000
									else
										acc = acc + stack[6] * 5; stack[6] = acc % 1000
									end
								else
									if op < 7 then
										acc = acc + stack[7] * 6; stack[7] = acc % 1000
									else
										acc = acc + stack[8] * 7; stack[1] = acc % 1000
									end
								end
							end
						else
							if op < 12 then
								if op < 10 then
									if op < 9 then
										acc = acc + stack[9] * 8; stack[2] = acc % 1000
									else
										acc = acc + stack[10] * 9; stack[3] = acc % 1000
									end
								else
									if op < 11 then
										acc = acc + stack[11] * 10; stack[4] = acc % 1000
									else
										acc = acc + stack[12] * 11; stack[5] = acc % 1000
									end
								end
							else
								if op < 14 then
									if op < 13 then
										acc = acc + stack[13] * 12; stack[6] = acc % 1000
									else
										acc = acc + stack[14] * 13; stack[7] = acc % 1000
									end
								else
									if op < 15 then
										acc = acc + stack[15] * 14; stack[1] = acc % 1000
									else
										acc = acc + stack[16] * 15; stack[2] = acc % 1000
									end
								end
							end
						end
					else
						if op < 24 then
							if op < 20 then
								if op < 18 then
									if op < 17 then
										acc = acc + stack[17] * 16; stack[3] = acc % 1000
									else
										acc = acc + stack[18] * 17; stack[4] = acc % 1000
									end
								else
									if op < 19 then
										acc = acc + stack[19] * 18; stack[5] = acc % 1000
									else
										acc = acc + stack[20] * 19; stack[6] = acc % 1000
									end
								end
							else
								if op < 22 then
									if op < 21 then
										acc = acc + stack[21] * 20; stack[7] = acc % 1000
									else
										acc = acc + stack[22] * 21; stack[1] = acc % 1000
									end
								else
									if op < 23 then
										acc = acc + stack[23] * 22; stack[2] = acc % 1000
									else
										acc = acc + stack[24] * 23; stack[3] = acc % 1000
									end
								end
							end
						else
							if op < 28 then
								if op < 26 then
									if op < 25 then
										acc = acc + stack[25] * 24; stack[4] = acc % 1000
									else
										acc = acc + stack[26] * 25; stack[5] = acc % 1000
									end
								else
									if op < 27 then
										acc = acc + stack[27] * 26; stack[6] = acc % 1000
									else
										acc = acc + stack[28] * 27; stack[7] = acc % 1000
									end
								end
							else
								if op < 30 then
									if op < 29 then
										acc = acc + stack[29] * 28; stack[1] = acc % 1000
									else
										acc = acc + stack[30] * 29; stack[2] = acc % 1000
									end
								else
									if op < 31 then
										acc = acc + stack[31] * 30; stack[3] = acc % 1000
									else
										acc = acc + stack[32] * 31; stack[4] = acc % 1000
									end
								end
							end
						end
					end
				else
					if op < 48 then
						if op < 40 then
							if op < 36 then
								if op < 34 then
									if op < 33 then
										acc = acc + stack[33] * 32; stack[5] = acc % 1000
									else
										acc = acc + stack[34] * 33; stack[6] = acc % 1000
									end
								else
									if op < 35 then
										acc = acc + stack[35] * 34; stack[7] = acc % 1000
									else
										acc = acc + stack[36] * 35; stack[1] = acc % 1000
									end
								end
							else
								if op < 38 then
									if op < 37 then
										acc = acc + stack[37] * 36; stack[2] = acc % 1000
									else
										acc = acc + stack[38] * 37; stack[3] = acc % 1000
									end
								else
									if op < 39 then
										acc = acc + stack[39] * 38; stack[4] = acc % 1000
									else
										acc = acc + stack[40] * 39; stack[5] = acc % 1000
									end
								end
							end
						else
							if op < 44 then
								if op < 42 then
									if op < 41 then
										acc = acc + stack[41] * 40; stack[6] = acc % 1000
									else
										acc = acc + stack[42] * 41; stack[7] = acc % 1000
									end
								else
									if op < 43 then
										acc = acc + stack[43] * 42; stack[1] = acc % 1000
									else
										acc = acc + stack[44] * 43; stack[2] = acc % 1000
									end
								end
							else
								if op < 46 then
									if op < 45 then
										acc = acc + stack[45] * 44; stack[3] = acc % 1000
									else
										acc = acc + stack[46] * 45; stack[4] = acc % 1000
									end
								else
									if op < 47 then
										acc = acc + stack[47] * 46; stack[5] = acc % 1000
									else
										acc = acc + stack[48] * 47; stack[6] = acc % 1000
									end
								end
							end
						end
					else
						if op < 56 then
							if op < 52 then
								if op < 50 then
									if op < 49 then
										acc = acc + stack[49] * 48; stack[7] = acc % 1000
									else
										acc = acc + stack[50] * 49; stack[1] = acc % 1000
									end
								else
									if op < 51 then
										acc = acc + stack[1] * 50; stack[2] = acc % 1000
									else
										acc = acc + stack[2] * 51; stack[3] = acc % 1000
									end
								end
							else
								if op < 54 then
									if op < 53 then
										acc = acc + stack[3] * 52; stack[4] = acc % 1000
									else
										acc = acc + stack[4] * 53; stack[5] = acc % 1000
									end
								else
									if op < 55 then
										acc = acc + stack[5] * 54; stack[6] = acc % 1000
									else
										acc = acc + stack[6] * 55; stack[7] = acc % 1000
									end
								end
							end
						else
							if op < 60 then
								if op < 58 then
									if op < 57 then
										acc = acc + stack[7] * 56; stack[1] = acc % 1000
									else
										acc = acc + stack[8] * 57; stack[2] = acc % 1000
									end
								else
									if op < 59 then
										acc = acc + stack[9] * 58; stack[3] = acc % 1000
									else
										acc = acc + stack[10] * 59; stack[4] = acc % 1000
									end
								end
							else
								if op < 62 then
									if op < 61 then
										acc = acc + stack[11] * 60; stack[5] = acc % 1000
									else
										acc = acc + stack[12] * 61; stack[6] = acc % 1000
									end
								else
									if op < 63 then
										acc = acc + stack[13] * 62; stack[7] = acc % 1000
									else
										acc = acc + stack[14] * 63; stack[1] = acc % 1000
									end
								end
							end
						end
					end
				end
			else
				if op < 96 then
					if op < 80 then
						if op < 72 then
							if op < 68 then
								if op < 66 then
									if op < 65 then
										acc = acc + stack[15] * 64; stack[2] = acc % 1000
									else
										acc = acc + stack[16] * 65; stack[3] = acc % 1000
									end
								else
									if op < 67 then
										acc = acc + stack[17] * 66; stack[4] = acc % 1000
									else
										acc = acc + stack[18] * 67; stack[5] = acc % 1000
									end
								end
							else
								if op < 70 then
									if op < 69 then
										acc = acc + stack[19] * 68; stack[6] = acc % 1000
									else
										acc = acc + stack[20] * 69; stack[7] = acc % 1000
									end
								else
									if op < 71 then
										acc = acc + stack[21] * 70; stack[1] = acc % 1000
									else
										acc = acc + stack[22] * 71; stack[2] = acc % 1000
									end
								end
							end
						else
							if op < 76 then
								if op < 74 then
									if op < 73 then
										acc = acc + stack[23] * 72; stack[3] = acc % 1000
									else
										acc = acc + stack[24] * 73; stack[4] = acc % 1000
									end
								else
									if op < 75 then
										acc = acc + stack[25] * 74; stack[5] = acc % 1000
									else
										acc = acc + stack[26] * 75; stack[6] = acc % 1000
									end
								end
							else
								if op < 78 then
									if op < 77 then
										acc = acc + stack[27] * 76; stack[7] = acc % 1000
									else
										acc = acc + stack[28] * 77; stack[1] = acc % 1000
									end
								else
									if op < 79 then
										acc = acc + stack[29] * 78; stack[2] = acc % 1000
									else
										acc = acc + stack[30] * 79; stack[3] = acc % 1000
									end
								end
							end
						end
					else
						if op < 88 then
							if op < 84 then
								if op < 82 then
									if op < 81 then
										acc = acc + stack[31] * 80; stack[4] = acc % 1000
									else
										acc = acc + stack[32] * 81; stack[5] = acc % 1000
									end
								else
									if op < 83 then
										acc = acc + stack[33] * 82; stack[6] = acc % 1000
									else
										acc = acc + stack[34] * 83; stack[7] = acc % 1000
									end
								end
							else
								if op < 86 then
									if op < 85 then
										acc = acc + stack[35] * 84; stack[1] = acc % 1000
									else
										acc = acc + stack[36] * 85; stack[2] = acc % 1000
									end
								else
									if op < 87 then
										acc = acc + stack[37] * 86; stack[3] = acc % 1000
									else
										acc = acc + stack[38] * 87; stack[4] = acc % 1000
									end
								end
							end
						else
							if op < 92 then
								if op < 90 then
									if op < 89 then
										acc = acc + stack[39] * 88; stack[5] = acc % 1000
									else
										acc = acc + stack[40] * 89; stack[6] = acc % 1000
									end
								else
									if op < 91 then
										acc = acc + stack[41] * 90; stack[7] = acc % 1000
									else
										acc = acc + stack[42] * 91; stack[1] = acc % 1000
									end
								end
							else
								if op < 94 then
									if op < 93 then
										acc = acc + stack[43] * 92; stack[2] = acc % 1000
									else
										acc = acc + stack[44] * 93; stack[3] = acc % 1000
									end
								else
									if op < 95 then
										acc = acc + stack[45] * 94; stack[4] = acc % 1000
									else
										acc = acc + stack[46] * 95; stack[5] = acc % 1000
									end
								end
							end
						end
					end
				else
					if op < 112 then
						if op < 104 then
							if op < 100 then
								if op < 98 then
									if op < 97 then
										acc = acc + stack[47] * 96; stack[6] = acc % 1000
									else
										acc = acc + stack[48] * 97; stack[7] = acc % 1000
									end
								else
									if op < 99 then
										acc = acc + stack[49] * 98; stack[1] = acc % 1000
									else
										acc = acc + stack[50] * 99; stack[2] = acc % 1000
									end
								end
							else
								if op < 102 then
									if op < 101 then
										acc = acc + stack[1] * 100; stack[3] = acc % 1000
									else
										acc = acc + stack[2] * 101; stack[4] = acc % 1000
									end
								else
									if op < 103 then
										acc = acc + stack[3] * 102; stack[5] = acc % 1000
									else
										acc = acc + stack[4] * 103; stack[6] = acc % 1000
									end
								end
							end
						else
							if op < 108 then
								if op < 106 then
									if op < 105 then
										acc = acc + stack[5] * 104; stack[7] = acc % 1000
									else
										acc = acc + stack[6] * 105; stack[1] = acc % 1000
									end
								else
									if op < 107 then
										acc = acc + stack[7] * 106; stack[2] = acc % 1000
									else
										acc = acc + stack[8] * 107; stack[3] = acc % 1000
									end
								end
							else
								if op < 110 then
									if op < 109 then
										acc = acc + stack[9] * 108; stack[4] = acc % 1000
									else
										acc = acc + stack[10] * 109; stack[5] = acc % 1000
									end
								else
									if op < 111 then
										acc = acc + stack[11] * 110; stack[6] = acc % 1000
									else
										acc = acc + stack[12] * 111; stack[7] = acc % 1000
									end
								end
							end
						end
					else
						if op < 120 then
							if op < 116 then
								if op < 114 then
									if op < 113 then
										acc = acc + stack[13] * 112; stack[1] = acc % 1000
									else
										acc = acc + stack[14] * 113; stack[2] = acc % 1000
									end
								else
									if op < 115 then
										acc = acc + stack[15] * 114; stack[3] = acc % 1000
									else
										acc = acc + stack[16] * 115; stack[4] = acc % 1000
									end
								end
							else
								if op < 118 then
									if op < 117 then
										acc = acc + stack[17] * 116; stack[5] = acc % 1000
									else
										acc = acc + stack[18] * 117; stack[6] = acc % 1000
									end
								else
									if op < 119 then
										acc = acc + stack[19] * 118; stack[7] = acc % 1000
									else
										acc = acc + stack[20] * 119; stack[1] = acc % 1000
									end
								end
							end
						else
							if op < 124 then
								if op < 122 then
									if op < 121 then
										acc = acc + stack[21] * 120; stack[2] = acc % 1000
									else
										acc = acc + stack[22] * 121; stack[3] = acc % 1000
									end
								else
									if op < 123 then
										acc = acc + stack[23] * 122; stack[4] = acc % 1000
									else
										acc = acc + stack[24] * 123; stack[5] = acc % 1000
									end
								end
							else
								if op < 126 then
									if op < 125 then
										acc = acc + stack[25] * 124; stack[6] = acc % 1000
									else
										acc = acc + stack[26] * 125; stack[7] = acc % 1000
									end
								else
									if op < 127 then
										acc = acc + stack[27] * 126; stack[1] = acc % 1000
									else
										acc = acc + stack[28] * 127; stack[2] = acc % 1000
									end
								end
							end
						end
					end
				end
			end
		else
			if op < 192 then
				if op < 160 then
					if op < 144 then
						if op < 136 then
							if op < 132 then
								if op < 130 then
									if op < 129 then
										acc = acc + stack[29] * 128; stack[3] = acc % 1000
									else
										acc = acc + stack[30] * 129; stack[4] = acc % 1000
									end
								else
									if op < 131 then
										acc = acc + stack[31] * 130; stack[5] = acc % 1000
									else
										acc = acc + stack[32] * 131; stack[6] = acc % 1000
									end
								end
							else
								if op < 134 then
									if op < 133 then
										acc = acc + stack[33] * 132; stack[7] = acc % 1000
									else
										acc = acc + stack[34] * 133; stack[1] = acc % 1000
									end
								else
									if op < 135 then
										acc = acc + stack[35] * 134; stack[2] = acc % 1000
									else
										acc = acc + stack[36] * 135; stack[3] = acc % 1000
									end
								end
							end
						else
							if op < 140 then
								if op < 138 then
									if op < 137 then
										acc = acc + stack[37] * 136; stack[4] = acc % 1000
									else
										acc = acc + stack[38] * 137; stack[5] = acc % 1000
									end
								else
									if op < 139 then
										acc = acc + stack[39] * 138; stack[6] = acc % 1000
									else
										acc = acc + stack[40] * 139; stack[7] = acc % 1000
									end
								end
							else
								if op < 142 then
									if op < 141 then
										acc = acc + stack[41] * 140; stack[1] = acc % 1000
									else
										acc = acc + stack[42] * 141; stack[2] = acc % 1000
									end
								else
									if op < 143 then
										acc = acc + stack[43] * 142; stack[3] = acc % 1000
									else
										acc = acc + stack[44] * 143; stack[4] = acc % 1000
									end
								end
							end
						end
					else
						if op < 152 then
							if op < 148 then
								if op < 146 then
									if op < 145 then
										acc = acc + stack[45] * 144; stack[5] = acc % 1000
									else
										acc = acc + stack[46] * 145; stack[6] = acc % 1000
									end
								else
									if op < 147 then
										acc = acc + stack[47] * 146; stack[7] = acc % 1000
									else
										acc = acc + stack[48] * 147; stack[1] = acc % 1000
									end
								end
							else
								if op < 150 then
									if op < 149 then
										acc = acc + stack[49] * 148; stack[2] = acc % 1000
									else
										acc = acc + stack[50] * 149; stack[3] = acc % 1000
									end
								else
									if op < 151 then
										acc = acc + stack[1] * 150; stack[4] = acc % 1000
									else
										acc = acc + stack[2] * 151; stack[5] = acc % 1000
									end
								end
							end
						else
							if op < 156 then
								if op < 154 then
									if op < 153 then
										acc = acc + stack[3] * 152; stack[6] = acc % 1000
									else
										acc = acc + stack[4] * 153; stack[7] = acc % 1000
									end
								else
									if op < 155 then
										acc = acc + stack[5] * 154; stack[1] = acc % 1000
									else
										acc = acc + stack[6] * 155; stack[2] = acc % 1000
									end
								end
							else
								if op < 158 then
									if op < 157 then
										acc = acc + stack[7] * 156; stack[3] = acc % 1000
									else
										acc = acc + stack[8] * 157; stack[4] = acc % 1000
									end
								else
									if op < 159 then
										acc = acc + stack[9] * 158; stack[5] = acc % 1000
									else
										acc = acc + stack[10] * 159; stack[6] = acc % 1000
									end
								end
							end
						end
					end
				else
					if op < 176 then
						if op < 168 then
							if op < 164 then
								if op < 162 then
									if op < 161 then
										acc = acc + stack[11] * 160; stack[7] = acc % 1000
									else
										acc = acc + stack[12] * 161; stack[1] = acc % 1000
									end
								else
									if op < 163 then
										acc = acc + stack[13] * 162; stack[2] = acc % 1000
									else
										acc = acc + stack[14] * 163; stack[3] = acc % 1000
									end
								end
							else
								if op < 166 then
									if op < 165 then
										acc = acc + stack[15] * 164; stack[4] = acc % 1000
									else
										acc = acc + stack[16] * 165; stack[5] = acc % 1000
									end
								else
									if op < 167 then
										acc = acc + stack[17] * 166; stack[6] = acc % 1000
									else
										acc = acc + stack[18] * 167; stack[7] = acc % 1000
									end
								end
							end
						else
							if op < 172 then
								if op < 170 then
									if op < 169 then
										acc = acc + stack[19] * 168; stack[1] = acc % 1000
									else
										acc = acc + stack[20] * 169; stack[2] = acc % 1000
									end
								else
									if op < 171 then
										acc = acc + stack[21] * 170; stack[3] = acc % 1000
									else
										acc = acc + stack[22] * 171; stack[4] = acc % 1000
									end
								end
							else
								if op < 174 then
									if op < 173 then
										acc = acc + stack[23] * 172; stack[5] = acc % 1000
									else
										acc = acc + stack[24] * 173; stack[6] = acc % 1000
									end
								else
									if op < 175 then
										acc = acc + stack[25] * 174; stack[7] = acc % 1000
									else
										acc = acc + stack[26] * 175; stack[1] = acc % 1000
									end
								end
							end
						end
					else
						if op < 184 then
							if op < 180 then
								if op < 178 then
									if op < 177 then
										acc = acc + stack[27] * 176; stack[2] = acc % 1000
									else
										acc = acc + stack[28] * 177; stack[3] = acc % 1000
									end
								else
									if op < 179 then
										acc = acc + stack[29] * 178; stack[4] = acc % 1000
									else
										acc = acc + stack[30] * 179; stack[5] = acc % 1000
									end
								end
							else
								if op < 182 then
									if op < 181 then
										acc = acc + stack[31] * 180; stack[6] = acc % 1000
									else
										acc = acc + stack[32] * 181; stack[7] = acc % 1000
									end
								else
									if op < 183 then
										acc = acc + stack[33] * 182; stack[1] = acc % 1000
									else
										acc = acc + stack[34] * 183; stack[2] = acc % 1000
									end
								end
							end
						else
							if op < 188 then
								if op < 186 then
									if op < 185 then
										acc = acc + stack[35] * 184; stack[3] = acc % 1000
									else
										acc = acc + stack[36] * 185; stack[4] = acc % 1000
									end
								else
									if op < 187 then
										acc = acc + stack[37] * 186; stack[5] = acc % 1000
									else
										acc = acc + stack[38] * 187; stack[6] = acc % 1000
									end
								end
							else
								if op < 190 then
									if op < 189 then
										acc = acc + stack[39] * 188; stack[7] = acc % 1000
									else
										acc = acc + stack[40] * 189; stack[1] = acc % 1000
									end
								else
									if op < 191 then
										acc = acc + stack[41] * 190; stack[2] = acc % 1000
									else
										acc = acc + stack[42] * 191; stack[3] = acc % 1000
									end
								end
							end
						end
					end
				end
			else
				if op < 224 then
					if op < 208 then
						if op < 200 then
							if op < 196 then
								if op < 194 then
									if op < 193 then
										acc = acc + stack[43] * 192; stack[4] = acc % 1000
									else
										acc = acc + stack[44] * 193; stack[5] = acc % 1000
									end
								else
									if op < 195 then
										acc = acc + stack[45] * 194; stack[6] = acc % 1000
									else
										acc = acc + stack[46] * 195; stack[7] = acc % 1000
									end
								end
							else
								if op < 198 then
									if op < 197 then
										acc = acc + stack[47] * 196; stack[1] = acc % 1000
									else
										acc = acc + stack[48] * 197; stack[2] = acc % 1000
									end
								else
									if op < 199 then
										acc = acc + stack[49] * 198; stack[3] = acc % 1000
									else
										acc = acc + stack[50] * 199; stack[4] = acc % 1000
									end
								end
							end
						else
							if op < 204 then
								if op < 202 then
									if op < 201 then
										acc = acc + stack[1] * 200; stack[5] = acc % 1000
									else
										acc = acc + stack[2] * 201; stack[6] = acc % 1000
									end
								else
									if op < 203 then
										acc = acc + stack[3] * 202; stack[7] = acc % 1000
									else
										acc = acc + stack[4] * 203; stack[1] = acc % 1000
									end
								end
							else
								if op < 206 then
									if op < 205 then
										acc = acc + stack[5] * 204; stack[2] = acc % 1000
									else
										acc = acc + stack[6] * 205; stack[3] = acc % 1000
									end
								else
									if op < 207 then
										acc = acc + stack[7] * 206; stack[4] = acc % 1000
									else
										acc = acc + stack[8] * 207; stack[5] = acc % 1000
									end
								end
							end
						end
					else
						if op < 216 then
							if op < 212 then
								if op < 210 then
									if op < 209 then
										acc = acc + stack[9] * 208; stack[6] = acc % 1000
									else
										acc = acc + stack[10] * 209; stack[7] = acc % 1000
									end
								else
									if op < 211 then
										acc = acc + stack[11] * 210; stack[1] = acc % 1000
									else
										acc = acc + stack[12] * 211; stack[2] = acc % 1000
									end
								end
							else
								if op < 214 then
									if op < 213 then
										acc = acc + stack[13] * 212; stack[3] = acc % 1000
									else
										acc = acc + stack[14] * 213; stack[4] = acc % 1000
									end
								else
									if op < 215 then
										acc = acc + stack[15] * 214; stack[5] = acc % 1000
									else
										acc = acc + stack[16] * 215; stack[6] = acc % 1000
									end
								end
							end
						else
							if op < 220 then
								if op < 218 then
									if op < 217 then
										acc = acc + stack[17] * 216; stack[7] = acc % 1000
									else
										acc = acc + stack[18] * 217; stack[1] = acc % 1000
									end
								else
									if op < 219 then
										acc = acc + stack[19] * 218; stack[2] = acc % 1000
									else
										acc = acc + stack[20] * 219; stack[3] = acc % 1000
									end
								end
							else
								if op < 222 then
									if op < 221 then
										acc = acc + stack[21] * 220; stack[4] = acc % 1000
									else
										acc = acc + stack[22] * 221; stack[5] = acc % 1000
									end
								else
									if op < 223 then
										acc = acc + stack[23] * 222; stack[6] = acc % 1000
									else
										acc = acc + stack[24] * 223; stack[7] = acc % 1000
									end
								end
							end
						end
					end
				else
					if op < 240 then
						if op < 232 then
							if op < 228 then
								if op < 226 then
									if op < 225 then
										acc = acc + stack[25] * 224; stack[1] = acc % 1000
									else
										acc = acc + stack[26] * 225; stack[2] = acc % 1000
									end
								else
									if op < 227 then
										acc = acc + stack[27] * 226; stack[3] = acc % 1000
									else
										acc = acc + stack[28] * 227; stack[4] = acc % 1000
									end
								end
							else
								if op < 230 then
									if op < 229 then
										acc = acc + stack[29] * 228; stack[5] = acc % 1000
									else
										acc = acc + stack[30] * 229; stack[6] = acc % 1000
									end
								else
									if op < 231 then
										acc = acc + stack[31] * 230; stack[7] = acc % 1000
									else
										acc = acc + stack[32] * 231; stack[1] = acc % 1000
									end
								end
							end
						else
							if op < 236 then
								if op < 234 then
									if op < 233 then
										acc = acc + stack[33] * 232; stack[2] = acc % 1000
									else
										acc = acc + stack[34] * 233; stack[3] = acc % 1000
									end
								else
									if op < 235 then
										acc = acc + stack[35] * 234; stack[4] = acc % 1000
									else
										acc = acc + stack[36] * 235; stack[5] = acc % 1000
									end
								end
							else
								if op < 238 then
									if op < 237 then
										acc = acc + stack[37] * 236; stack[6] = acc % 1000
									else
										acc = acc + stack[38] * 237; stack[7] = acc % 1000
									end
								else
									if op < 239 then
										acc = acc + stack[39] * 238; stack[1] = acc % 1000
									else
										acc = acc + stack[40] * 239; stack[2] = acc % 1000
									end
								end
							end
						end
					else
						if op < 248 then
							if op < 244 then
								if op < 242 then
									if op < 241 then
										acc = acc + stack[41] * 240; stack[3] = acc % 1000
									else
										acc = acc + stack[42] * 241; stack[4] = acc % 1000
									end
								else
									if op < 243 then
										acc = acc + stack[43] * 242; stack[5] = acc % 1000
									else
										acc = acc + stack[44] * 243; stack[6] = acc % 1000
									end
								end
							else
								if op < 246 then
									if op < 245 then
										acc = acc + stack[45] * 244; stack[7] = acc % 1000
									else
										acc = acc + stack[46] * 245; stack[1] = acc % 1000
									end
								else
									if op < 247 then
										acc = acc + stack[47] * 246; stack[2] = acc % 1000
									else
										acc = acc + stack[48] * 247; stack[3] = acc % 1000
									end
								end
							end
						else
							if op < 252 then
								if op < 250 then
									if op < 249 then
										acc = acc + stack[49] * 248; stack[4] = acc % 1000
									else
										acc = acc + stack[50] * 249; stack[5] = acc % 1000
									end
								else
									if op < 251 then
										acc = acc + stack[1] * 250; stack[6] = acc % 1000
									else
										acc = acc + stack[2] * 251; stack[7] = acc % 1000
									end
								end
							else
								if op < 254 then
									if op < 253 then
										acc = acc + stack[3] * 252; stack[1] = acc % 1000
									else
										acc = acc + stack[4] * 253; stack[2] = acc % 1000
									end
								else
									if op < 255 then
										acc = acc + stack[5] * 254; stack[3] = acc % 1000
									else
										acc = acc + stack[6] * 255; stack[4] = acc % 1000
									end
								end
							end
						end
					end
				end
			end
		end
		pc = pc + 1
	end
	return acc
end
local code = {}
for i = 1, 300 do
	code[i] = (i * 37) % 256
end
print(run(code), stack[1], stack[7])
