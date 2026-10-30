-- 210 `do` blocks each declaring a value and a closure capturing it: the
-- flattened output needed 420 locals at once (Luau allows 200) and was
-- refused; the output now puts the blocks back.
local base = tonumber("3")
local M = {}
do
	local u = base + 0
	local function c0(a)
		return a + u * 0
	end
	M[0] = c0
end
do
	local u = base + 1
	local function c1(a)
		return a + u * 1
	end
	M[1] = c1
end
do
	local u = base + 2
	local function c2(a)
		return a + u * 2
	end
	M[2] = c2
end
do
	local u = base + 3
	local function c3(a)
		return a + u * 3
	end
	M[3] = c3
end
do
	local u = base + 4
	local function c4(a)
		return a + u * 4
	end
	M[4] = c4
end
do
	local u = base + 5
	local function c5(a)
		return a + u * 5
	end
	M[5] = c5
end
do
	local u = base + 6
	local function c6(a)
		return a + u * 6
	end
	M[6] = c6
end
do
	local u = base + 7
	local function c7(a)
		return a + u * 0
	end
	M[7] = c7
end
do
	local u = base + 8
	local function c8(a)
		return a + u * 1
	end
	M[8] = c8
end
do
	local u = base + 9
	local function c9(a)
		return a + u * 2
	end
	M[9] = c9
end
do
	local u = base + 10
	local function c10(a)
		return a + u * 3
	end
	M[10] = c10
end
do
	local u = base + 11
	local function c11(a)
		return a + u * 4
	end
	M[11] = c11
end
do
	local u = base + 12
	local function c12(a)
		return a + u * 5
	end
	M[12] = c12
end
do
	local u = base + 13
	local function c13(a)
		return a + u * 6
	end
	M[13] = c13
end
do
	local u = base + 14
	local function c14(a)
		return a + u * 0
	end
	M[14] = c14
end
do
	local u = base + 15
	local function c15(a)
		return a + u * 1
	end
	M[15] = c15
end
do
	local u = base + 16
	local function c16(a)
		return a + u * 2
	end
	M[16] = c16
end
do
	local u = base + 17
	local function c17(a)
		return a + u * 3
	end
	M[17] = c17
end
do
	local u = base + 18
	local function c18(a)
		return a + u * 4
	end
	M[18] = c18
end
do
	local u = base + 19
	local function c19(a)
		return a + u * 5
	end
	M[19] = c19
end
do
	local u = base + 20
	local function c20(a)
		return a + u * 6
	end
	M[20] = c20
end
do
	local u = base + 21
	local function c21(a)
		return a + u * 0
	end
	M[21] = c21
end
do
	local u = base + 22
	local function c22(a)
		return a + u * 1
	end
	M[22] = c22
end
do
	local u = base + 23
	local function c23(a)
		return a + u * 2
	end
	M[23] = c23
end
do
	local u = base + 24
	local function c24(a)
		return a + u * 3
	end
	M[24] = c24
end
do
	local u = base + 25
	local function c25(a)
		return a + u * 4
	end
	M[25] = c25
end
do
	local u = base + 26
	local function c26(a)
		return a + u * 5
	end
	M[26] = c26
end
do
	local u = base + 27
	local function c27(a)
		return a + u * 6
	end
	M[27] = c27
end
do
	local u = base + 28
	local function c28(a)
		return a + u * 0
	end
	M[28] = c28
end
do
	local u = base + 29
	local function c29(a)
		return a + u * 1
	end
	M[29] = c29
end
do
	local u = base + 30
	local function c30(a)
		return a + u * 2
	end
	M[30] = c30
end
do
	local u = base + 31
	local function c31(a)
		return a + u * 3
	end
	M[31] = c31
end
do
	local u = base + 32
	local function c32(a)
		return a + u * 4
	end
	M[32] = c32
end
do
	local u = base + 33
	local function c33(a)
		return a + u * 5
	end
	M[33] = c33
end
do
	local u = base + 34
	local function c34(a)
		return a + u * 6
	end
	M[34] = c34
end
do
	local u = base + 35
	local function c35(a)
		return a + u * 0
	end
	M[35] = c35
end
do
	local u = base + 36
	local function c36(a)
		return a + u * 1
	end
	M[36] = c36
end
do
	local u = base + 37
	local function c37(a)
		return a + u * 2
	end
	M[37] = c37
end
do
	local u = base + 38
	local function c38(a)
		return a + u * 3
	end
	M[38] = c38
end
do
	local u = base + 39
	local function c39(a)
		return a + u * 4
	end
	M[39] = c39
end
do
	local u = base + 40
	local function c40(a)
		return a + u * 5
	end
	M[40] = c40
end
do
	local u = base + 41
	local function c41(a)
		return a + u * 6
	end
	M[41] = c41
end
do
	local u = base + 42
	local function c42(a)
		return a + u * 0
	end
	M[42] = c42
end
do
	local u = base + 43
	local function c43(a)
		return a + u * 1
	end
	M[43] = c43
end
do
	local u = base + 44
	local function c44(a)
		return a + u * 2
	end
	M[44] = c44
end
do
	local u = base + 45
	local function c45(a)
		return a + u * 3
	end
	M[45] = c45
end
do
	local u = base + 46
	local function c46(a)
		return a + u * 4
	end
	M[46] = c46
end
do
	local u = base + 47
	local function c47(a)
		return a + u * 5
	end
	M[47] = c47
end
do
	local u = base + 48
	local function c48(a)
		return a + u * 6
	end
	M[48] = c48
end
do
	local u = base + 49
	local function c49(a)
		return a + u * 0
	end
	M[49] = c49
end
do
	local u = base + 50
	local function c50(a)
		return a + u * 1
	end
	M[50] = c50
end
do
	local u = base + 51
	local function c51(a)
		return a + u * 2
	end
	M[51] = c51
end
do
	local u = base + 52
	local function c52(a)
		return a + u * 3
	end
	M[52] = c52
end
do
	local u = base + 53
	local function c53(a)
		return a + u * 4
	end
	M[53] = c53
end
do
	local u = base + 54
	local function c54(a)
		return a + u * 5
	end
	M[54] = c54
end
do
	local u = base + 55
	local function c55(a)
		return a + u * 6
	end
	M[55] = c55
end
do
	local u = base + 56
	local function c56(a)
		return a + u * 0
	end
	M[56] = c56
end
do
	local u = base + 57
	local function c57(a)
		return a + u * 1
	end
	M[57] = c57
end
do
	local u = base + 58
	local function c58(a)
		return a + u * 2
	end
	M[58] = c58
end
do
	local u = base + 59
	local function c59(a)
		return a + u * 3
	end
	M[59] = c59
end
do
	local u = base + 60
	local function c60(a)
		return a + u * 4
	end
	M[60] = c60
end
do
	local u = base + 61
	local function c61(a)
		return a + u * 5
	end
	M[61] = c61
end
do
	local u = base + 62
	local function c62(a)
		return a + u * 6
	end
	M[62] = c62
end
do
	local u = base + 63
	local function c63(a)
		return a + u * 0
	end
	M[63] = c63
end
do
	local u = base + 64
	local function c64(a)
		return a + u * 1
	end
	M[64] = c64
end
do
	local u = base + 65
	local function c65(a)
		return a + u * 2
	end
	M[65] = c65
end
do
	local u = base + 66
	local function c66(a)
		return a + u * 3
	end
	M[66] = c66
end
do
	local u = base + 67
	local function c67(a)
		return a + u * 4
	end
	M[67] = c67
end
do
	local u = base + 68
	local function c68(a)
		return a + u * 5
	end
	M[68] = c68
end
do
	local u = base + 69
	local function c69(a)
		return a + u * 6
	end
	M[69] = c69
end
do
	local u = base + 70
	local function c70(a)
		return a + u * 0
	end
	M[70] = c70
end
do
	local u = base + 71
	local function c71(a)
		return a + u * 1
	end
	M[71] = c71
end
do
	local u = base + 72
	local function c72(a)
		return a + u * 2
	end
	M[72] = c72
end
do
	local u = base + 73
	local function c73(a)
		return a + u * 3
	end
	M[73] = c73
end
do
	local u = base + 74
	local function c74(a)
		return a + u * 4
	end
	M[74] = c74
end
do
	local u = base + 75
	local function c75(a)
		return a + u * 5
	end
	M[75] = c75
end
do
	local u = base + 76
	local function c76(a)
		return a + u * 6
	end
	M[76] = c76
end
do
	local u = base + 77
	local function c77(a)
		return a + u * 0
	end
	M[77] = c77
end
do
	local u = base + 78
	local function c78(a)
		return a + u * 1
	end
	M[78] = c78
end
do
	local u = base + 79
	local function c79(a)
		return a + u * 2
	end
	M[79] = c79
end
do
	local u = base + 80
	local function c80(a)
		return a + u * 3
	end
	M[80] = c80
end
do
	local u = base + 81
	local function c81(a)
		return a + u * 4
	end
	M[81] = c81
end
do
	local u = base + 82
	local function c82(a)
		return a + u * 5
	end
	M[82] = c82
end
do
	local u = base + 83
	local function c83(a)
		return a + u * 6
	end
	M[83] = c83
end
do
	local u = base + 84
	local function c84(a)
		return a + u * 0
	end
	M[84] = c84
end
do
	local u = base + 85
	local function c85(a)
		return a + u * 1
	end
	M[85] = c85
end
do
	local u = base + 86
	local function c86(a)
		return a + u * 2
	end
	M[86] = c86
end
do
	local u = base + 87
	local function c87(a)
		return a + u * 3
	end
	M[87] = c87
end
do
	local u = base + 88
	local function c88(a)
		return a + u * 4
	end
	M[88] = c88
end
do
	local u = base + 89
	local function c89(a)
		return a + u * 5
	end
	M[89] = c89
end
do
	local u = base + 90
	local function c90(a)
		return a + u * 6
	end
	M[90] = c90
end
do
	local u = base + 91
	local function c91(a)
		return a + u * 0
	end
	M[91] = c91
end
do
	local u = base + 92
	local function c92(a)
		return a + u * 1
	end
	M[92] = c92
end
do
	local u = base + 93
	local function c93(a)
		return a + u * 2
	end
	M[93] = c93
end
do
	local u = base + 94
	local function c94(a)
		return a + u * 3
	end
	M[94] = c94
end
do
	local u = base + 95
	local function c95(a)
		return a + u * 4
	end
	M[95] = c95
end
do
	local u = base + 96
	local function c96(a)
		return a + u * 5
	end
	M[96] = c96
end
do
	local u = base + 97
	local function c97(a)
		return a + u * 6
	end
	M[97] = c97
end
do
	local u = base + 98
	local function c98(a)
		return a + u * 0
	end
	M[98] = c98
end
do
	local u = base + 99
	local function c99(a)
		return a + u * 1
	end
	M[99] = c99
end
do
	local u = base + 100
	local function c100(a)
		return a + u * 2
	end
	M[100] = c100
end
do
	local u = base + 101
	local function c101(a)
		return a + u * 3
	end
	M[101] = c101
end
do
	local u = base + 102
	local function c102(a)
		return a + u * 4
	end
	M[102] = c102
end
do
	local u = base + 103
	local function c103(a)
		return a + u * 5
	end
	M[103] = c103
end
do
	local u = base + 104
	local function c104(a)
		return a + u * 6
	end
	M[104] = c104
end
do
	local u = base + 105
	local function c105(a)
		return a + u * 0
	end
	M[105] = c105
end
do
	local u = base + 106
	local function c106(a)
		return a + u * 1
	end
	M[106] = c106
end
do
	local u = base + 107
	local function c107(a)
		return a + u * 2
	end
	M[107] = c107
end
do
	local u = base + 108
	local function c108(a)
		return a + u * 3
	end
	M[108] = c108
end
do
	local u = base + 109
	local function c109(a)
		return a + u * 4
	end
	M[109] = c109
end
do
	local u = base + 110
	local function c110(a)
		return a + u * 5
	end
	M[110] = c110
end
do
	local u = base + 111
	local function c111(a)
		return a + u * 6
	end
	M[111] = c111
end
do
	local u = base + 112
	local function c112(a)
		return a + u * 0
	end
	M[112] = c112
end
do
	local u = base + 113
	local function c113(a)
		return a + u * 1
	end
	M[113] = c113
end
do
	local u = base + 114
	local function c114(a)
		return a + u * 2
	end
	M[114] = c114
end
do
	local u = base + 115
	local function c115(a)
		return a + u * 3
	end
	M[115] = c115
end
do
	local u = base + 116
	local function c116(a)
		return a + u * 4
	end
	M[116] = c116
end
do
	local u = base + 117
	local function c117(a)
		return a + u * 5
	end
	M[117] = c117
end
do
	local u = base + 118
	local function c118(a)
		return a + u * 6
	end
	M[118] = c118
end
do
	local u = base + 119
	local function c119(a)
		return a + u * 0
	end
	M[119] = c119
end
do
	local u = base + 120
	local function c120(a)
		return a + u * 1
	end
	M[120] = c120
end
do
	local u = base + 121
	local function c121(a)
		return a + u * 2
	end
	M[121] = c121
end
do
	local u = base + 122
	local function c122(a)
		return a + u * 3
	end
	M[122] = c122
end
do
	local u = base + 123
	local function c123(a)
		return a + u * 4
	end
	M[123] = c123
end
do
	local u = base + 124
	local function c124(a)
		return a + u * 5
	end
	M[124] = c124
end
do
	local u = base + 125
	local function c125(a)
		return a + u * 6
	end
	M[125] = c125
end
do
	local u = base + 126
	local function c126(a)
		return a + u * 0
	end
	M[126] = c126
end
do
	local u = base + 127
	local function c127(a)
		return a + u * 1
	end
	M[127] = c127
end
do
	local u = base + 128
	local function c128(a)
		return a + u * 2
	end
	M[128] = c128
end
do
	local u = base + 129
	local function c129(a)
		return a + u * 3
	end
	M[129] = c129
end
do
	local u = base + 130
	local function c130(a)
		return a + u * 4
	end
	M[130] = c130
end
do
	local u = base + 131
	local function c131(a)
		return a + u * 5
	end
	M[131] = c131
end
do
	local u = base + 132
	local function c132(a)
		return a + u * 6
	end
	M[132] = c132
end
do
	local u = base + 133
	local function c133(a)
		return a + u * 0
	end
	M[133] = c133
end
do
	local u = base + 134
	local function c134(a)
		return a + u * 1
	end
	M[134] = c134
end
do
	local u = base + 135
	local function c135(a)
		return a + u * 2
	end
	M[135] = c135
end
do
	local u = base + 136
	local function c136(a)
		return a + u * 3
	end
	M[136] = c136
end
do
	local u = base + 137
	local function c137(a)
		return a + u * 4
	end
	M[137] = c137
end
do
	local u = base + 138
	local function c138(a)
		return a + u * 5
	end
	M[138] = c138
end
do
	local u = base + 139
	local function c139(a)
		return a + u * 6
	end
	M[139] = c139
end
do
	local u = base + 140
	local function c140(a)
		return a + u * 0
	end
	M[140] = c140
end
do
	local u = base + 141
	local function c141(a)
		return a + u * 1
	end
	M[141] = c141
end
do
	local u = base + 142
	local function c142(a)
		return a + u * 2
	end
	M[142] = c142
end
do
	local u = base + 143
	local function c143(a)
		return a + u * 3
	end
	M[143] = c143
end
do
	local u = base + 144
	local function c144(a)
		return a + u * 4
	end
	M[144] = c144
end
do
	local u = base + 145
	local function c145(a)
		return a + u * 5
	end
	M[145] = c145
end
do
	local u = base + 146
	local function c146(a)
		return a + u * 6
	end
	M[146] = c146
end
do
	local u = base + 147
	local function c147(a)
		return a + u * 0
	end
	M[147] = c147
end
do
	local u = base + 148
	local function c148(a)
		return a + u * 1
	end
	M[148] = c148
end
do
	local u = base + 149
	local function c149(a)
		return a + u * 2
	end
	M[149] = c149
end
do
	local u = base + 150
	local function c150(a)
		return a + u * 3
	end
	M[150] = c150
end
do
	local u = base + 151
	local function c151(a)
		return a + u * 4
	end
	M[151] = c151
end
do
	local u = base + 152
	local function c152(a)
		return a + u * 5
	end
	M[152] = c152
end
do
	local u = base + 153
	local function c153(a)
		return a + u * 6
	end
	M[153] = c153
end
do
	local u = base + 154
	local function c154(a)
		return a + u * 0
	end
	M[154] = c154
end
do
	local u = base + 155
	local function c155(a)
		return a + u * 1
	end
	M[155] = c155
end
do
	local u = base + 156
	local function c156(a)
		return a + u * 2
	end
	M[156] = c156
end
do
	local u = base + 157
	local function c157(a)
		return a + u * 3
	end
	M[157] = c157
end
do
	local u = base + 158
	local function c158(a)
		return a + u * 4
	end
	M[158] = c158
end
do
	local u = base + 159
	local function c159(a)
		return a + u * 5
	end
	M[159] = c159
end
do
	local u = base + 160
	local function c160(a)
		return a + u * 6
	end
	M[160] = c160
end
do
	local u = base + 161
	local function c161(a)
		return a + u * 0
	end
	M[161] = c161
end
do
	local u = base + 162
	local function c162(a)
		return a + u * 1
	end
	M[162] = c162
end
do
	local u = base + 163
	local function c163(a)
		return a + u * 2
	end
	M[163] = c163
end
do
	local u = base + 164
	local function c164(a)
		return a + u * 3
	end
	M[164] = c164
end
do
	local u = base + 165
	local function c165(a)
		return a + u * 4
	end
	M[165] = c165
end
do
	local u = base + 166
	local function c166(a)
		return a + u * 5
	end
	M[166] = c166
end
do
	local u = base + 167
	local function c167(a)
		return a + u * 6
	end
	M[167] = c167
end
do
	local u = base + 168
	local function c168(a)
		return a + u * 0
	end
	M[168] = c168
end
do
	local u = base + 169
	local function c169(a)
		return a + u * 1
	end
	M[169] = c169
end
do
	local u = base + 170
	local function c170(a)
		return a + u * 2
	end
	M[170] = c170
end
do
	local u = base + 171
	local function c171(a)
		return a + u * 3
	end
	M[171] = c171
end
do
	local u = base + 172
	local function c172(a)
		return a + u * 4
	end
	M[172] = c172
end
do
	local u = base + 173
	local function c173(a)
		return a + u * 5
	end
	M[173] = c173
end
do
	local u = base + 174
	local function c174(a)
		return a + u * 6
	end
	M[174] = c174
end
do
	local u = base + 175
	local function c175(a)
		return a + u * 0
	end
	M[175] = c175
end
do
	local u = base + 176
	local function c176(a)
		return a + u * 1
	end
	M[176] = c176
end
do
	local u = base + 177
	local function c177(a)
		return a + u * 2
	end
	M[177] = c177
end
do
	local u = base + 178
	local function c178(a)
		return a + u * 3
	end
	M[178] = c178
end
do
	local u = base + 179
	local function c179(a)
		return a + u * 4
	end
	M[179] = c179
end
do
	local u = base + 180
	local function c180(a)
		return a + u * 5
	end
	M[180] = c180
end
do
	local u = base + 181
	local function c181(a)
		return a + u * 6
	end
	M[181] = c181
end
do
	local u = base + 182
	local function c182(a)
		return a + u * 0
	end
	M[182] = c182
end
do
	local u = base + 183
	local function c183(a)
		return a + u * 1
	end
	M[183] = c183
end
do
	local u = base + 184
	local function c184(a)
		return a + u * 2
	end
	M[184] = c184
end
do
	local u = base + 185
	local function c185(a)
		return a + u * 3
	end
	M[185] = c185
end
do
	local u = base + 186
	local function c186(a)
		return a + u * 4
	end
	M[186] = c186
end
do
	local u = base + 187
	local function c187(a)
		return a + u * 5
	end
	M[187] = c187
end
do
	local u = base + 188
	local function c188(a)
		return a + u * 6
	end
	M[188] = c188
end
do
	local u = base + 189
	local function c189(a)
		return a + u * 0
	end
	M[189] = c189
end
do
	local u = base + 190
	local function c190(a)
		return a + u * 1
	end
	M[190] = c190
end
do
	local u = base + 191
	local function c191(a)
		return a + u * 2
	end
	M[191] = c191
end
do
	local u = base + 192
	local function c192(a)
		return a + u * 3
	end
	M[192] = c192
end
do
	local u = base + 193
	local function c193(a)
		return a + u * 4
	end
	M[193] = c193
end
do
	local u = base + 194
	local function c194(a)
		return a + u * 5
	end
	M[194] = c194
end
do
	local u = base + 195
	local function c195(a)
		return a + u * 6
	end
	M[195] = c195
end
do
	local u = base + 196
	local function c196(a)
		return a + u * 0
	end
	M[196] = c196
end
do
	local u = base + 197
	local function c197(a)
		return a + u * 1
	end
	M[197] = c197
end
do
	local u = base + 198
	local function c198(a)
		return a + u * 2
	end
	M[198] = c198
end
do
	local u = base + 199
	local function c199(a)
		return a + u * 3
	end
	M[199] = c199
end
do
	local u = base + 200
	local function c200(a)
		return a + u * 4
	end
	M[200] = c200
end
do
	local u = base + 201
	local function c201(a)
		return a + u * 5
	end
	M[201] = c201
end
do
	local u = base + 202
	local function c202(a)
		return a + u * 6
	end
	M[202] = c202
end
do
	local u = base + 203
	local function c203(a)
		return a + u * 0
	end
	M[203] = c203
end
do
	local u = base + 204
	local function c204(a)
		return a + u * 1
	end
	M[204] = c204
end
do
	local u = base + 205
	local function c205(a)
		return a + u * 2
	end
	M[205] = c205
end
do
	local u = base + 206
	local function c206(a)
		return a + u * 3
	end
	M[206] = c206
end
do
	local u = base + 207
	local function c207(a)
		return a + u * 4
	end
	M[207] = c207
end
do
	local u = base + 208
	local function c208(a)
		return a + u * 5
	end
	M[208] = c208
end
do
	local u = base + 209
	local function c209(a)
		return a + u * 6
	end
	M[209] = c209
end
local total = 0
for i = 0, 209 do
	total += M[i](i)
end
print(total, M[0](1), M[209](2))
