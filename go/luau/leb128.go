package luau

import "errors"

func ReadULEB128(b []byte) (uint64, int, error) {
	var result uint64
	var shift uint
	for i, by := range b {
		if i == 9 && by > 1 {
			return 0, 0, errors.New("leb128: overflow")
		}
		if shift >= 64 && by&0x7f != 0 {
			return 0, 0, errors.New("leb128: overflow")
		}
		result |= uint64(by&0x7f) << shift
		shift += 7
		if by&0x80 == 0 {
			return result, i + 1, nil
		}
	}
	return 0, 0, errors.New("leb128: truncated")
}

func ReadString(b []byte) ([]byte, int, error) {
	n, sz, err := ReadULEB128(b)
	if err != nil {
		return nil, 0, err
	}
	if uint64(len(b)-sz) < n {
		return nil, 0, errors.New("string: truncated")
	}
	return b[sz : sz+int(n)], sz + int(n), nil
}

func ReadListCount(b []byte) (int, int, error) {
	n, sz, err := ReadULEB128(b)
	if err != nil {
		return 0, 0, err
	}
	if n > uint64(len(b)-sz) {
		return 0, 0, errors.New("list: count exceeds input")
	}
	return int(n), sz, nil
}
