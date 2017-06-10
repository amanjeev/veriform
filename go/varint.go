package zser

import (
	"bytes"
	"encoding/binary"
	"errors"
	"io"
)

// EncodeVarint encodes a uint64 into buf and returns the number of bytes written.
// If the buffer is too small, EncodeVarint will panic.
func EncodeVarint(buf []byte, input uint64) int {
	output := new(bytes.Buffer)
	length := 1
	result := (input << 1) | 1
	max := uint64(1 << 7)

	for input >= max {
		// 9-byte special case
		if length == 8 {
			output.WriteByte(0)
			binary.Write(output, binary.LittleEndian, input)
			copy(buf, output.Bytes())

			return 9
		}

		result <<= 1
		max <<= 7
		length += 1
	}

	binary.Write(output, binary.LittleEndian, result)
	copy(buf, output.Bytes()[0:length])

	return length
}

func DecodeVarint(r io.Reader) (uint64, error) {
	var result uint64
	var buf [8]byte

	_, err := r.Read(buf[:1])
	if err != nil {
		return 0, err
	}

	prefix := buf[0]
	if prefix == 0 {
		err := binary.Read(r, binary.LittleEndian, &result)

		if err == nil && result < (1 << 56) {
			return 0, errors.New("malformed varint")
		}

		return result, err
	}

	length := uint(1)

	// TODO: use math/bits TrailingZeros() if/when it becomes available
	// See: https://github.com/golang/go/issues/18616
	for prefix&1 == 0 {
		length += 1
		prefix >>= 1
	}

	_, err = io.ReadFull(r, buf[1:length])
	if err != nil {
		return 0, err
	}

	err = binary.Read(bytes.NewReader(buf[:]), binary.LittleEndian, &result)
	if err != nil {
		return 0, err
	}

	result >>= length
	if length > 1 && result < (1 << (7 * (length - 1))) {
		return 0, errors.New("malformed varint")
	}

	return result, nil
}
