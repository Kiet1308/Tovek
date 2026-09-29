// Package luau ports the Rust luau-lifter crate core (bytecode deserializer,
// opcodes, lifter, decompile API) to Go using only the standard library.
package luau

import (
	"fmt"
	"strconv"
	"strings"
	"unicode"
)

// Flag bits, matching luau-lifter's DONT_REUSE_VAR / NO_SYNTH_HELPERS /
// ASSUME_NO_NAN / STRICT_NO_SYNTHETIC_CONTROL / EMIT_BINDING_PROVENANCE /
// SYNTHESIZE_ARITHMETIC_LOOPS / COMPACT_ANNOTATIONS.
const (
	BitDontReuseVar              uint32 = 1 << 0
	BitNoSynthHelpers            uint32 = 1 << 1
	BitAssumeNoNaN               uint32 = 1 << 2
	BitStrictNoSyntheticControl  uint32 = 1 << 3
	BitEmitBindingProvenance     uint32 = 1 << 4
	BitSynthesizeArithmeticLoops uint32 = 1 << 5
	BitCompactAnnotations        uint32 = 1 << 6

	// KnownBits masks every flag bit a client may legally set.
	KnownBits uint32 = BitDontReuseVar | BitNoSynthHelpers | BitAssumeNoNaN |
		BitStrictNoSyntheticControl | BitEmitBindingProvenance |
		BitSynthesizeArithmeticLoops | BitCompactAnnotations
)

// DecompileOptions mirrors luau-lifter's DecompileOptions.
type DecompileOptions struct {
	DontReuseVar              bool
	NoSynthHelpers            bool
	AssumeNoNaN               bool
	StrictNoSyntheticControl  bool
	EmitBindingProvenance     bool
	SynthesizeArithmeticLoops bool
	CompactAnnotations        bool
}

// OptionsFromBits decodes a flag word; unknown bits are ignored, so callers
// must mask with KnownBits first when they need to reject them.
func OptionsFromBits(bits uint32) DecompileOptions {
	return DecompileOptions{
		DontReuseVar:              bits&BitDontReuseVar != 0,
		NoSynthHelpers:            bits&BitNoSynthHelpers != 0,
		AssumeNoNaN:               bits&BitAssumeNoNaN != 0,
		StrictNoSyntheticControl:  bits&BitStrictNoSyntheticControl != 0,
		EmitBindingProvenance:     bits&BitEmitBindingProvenance != 0,
		SynthesizeArithmeticLoops: bits&BitSynthesizeArithmeticLoops != 0,
		CompactAnnotations:        bits&BitCompactAnnotations != 0,
	}
}

// Union ORs two option sets, matching DecompileOptions::union.
func (o DecompileOptions) Union(other DecompileOptions) DecompileOptions {
	return DecompileOptions{
		DontReuseVar:              o.DontReuseVar || other.DontReuseVar,
		NoSynthHelpers:            o.NoSynthHelpers || other.NoSynthHelpers,
		AssumeNoNaN:               o.AssumeNoNaN || other.AssumeNoNaN,
		StrictNoSyntheticControl:  o.StrictNoSyntheticControl || other.StrictNoSyntheticControl,
		EmitBindingProvenance:     o.EmitBindingProvenance || other.EmitBindingProvenance,
		SynthesizeArithmeticLoops: o.SynthesizeArithmeticLoops || other.SynthesizeArithmeticLoops,
		CompactAnnotations:        o.CompactAnnotations || other.CompactAnnotations,
	}
}

// Bits returns the flag word for this option set, matching
// DecompileOptions::bits.
func (o DecompileOptions) Bits() uint32 {
	return OptionsBits(o)
}

// OptionsBits encodes an option set as a flag word.
func OptionsBits(o DecompileOptions) uint32 {
	var b uint32
	if o.DontReuseVar {
		b |= BitDontReuseVar
	}
	if o.NoSynthHelpers {
		b |= BitNoSynthHelpers
	}
	if o.AssumeNoNaN {
		b |= BitAssumeNoNaN
	}
	if o.StrictNoSyntheticControl {
		b |= BitStrictNoSyntheticControl
	}
	if o.EmitBindingProvenance {
		b |= BitEmitBindingProvenance
	}
	if o.SynthesizeArithmeticLoops {
		b |= BitSynthesizeArithmeticLoops
	}
	if o.CompactAnnotations {
		b |= BitCompactAnnotations
	}
	return b
}

// ParseFlagsText parses "" (defaults), decimal flag bits (unknown bits are
// rejected), or a token list of NONE|DONT_REUSE_VAR|STRICT_NO_SYNTHETIC_CONTROL
// (case-insensitive, '-' interchangeable with '_', separated by ',', ';' or
// whitespace). It mirrors DecompileOptions::from_flag_bits plus the
// server/worker parse_flags_text token path.
func ParseFlagsText(raw string) (DecompileOptions, error) {
	s := strings.TrimSpace(raw)
	if s == "" {
		return DecompileOptions{}, nil
	}
	if bits, err := strconv.ParseUint(s, 10, 32); err == nil {
		if uint32(bits)&^KnownBits != 0 {
			return DecompileOptions{}, fmt.Errorf("unsupported decompile flag bits: %d", bits)
		}
		return OptionsFromBits(uint32(bits)), nil
	}
	var opts DecompileOptions
	for _, tok := range strings.FieldsFunc(s, func(r rune) bool {
		return r == ',' || r == ';' || unicode.IsSpace(r)
	}) {
		switch strings.ToUpper(strings.ReplaceAll(strings.TrimSpace(tok), "-", "_")) {
		case "", "NONE":
		case "DONT_REUSE_VAR":
			opts.DontReuseVar = true
		case "STRICT_NO_SYNTHETIC_CONTROL":
			opts.StrictNoSyntheticControl = true
		default:
			return DecompileOptions{}, fmt.Errorf("unsupported decompile flag: %s", tok)
		}
	}
	return opts, nil
}

// ParseBool parses 1/true/yes/on as true and 0/false/no/off as false.
func ParseBool(raw string) (bool, error) {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case "1", "true", "yes", "on":
		return true, nil
	case "0", "false", "no", "off":
		return false, nil
	default:
		return false, fmt.Errorf("%q must be a boolean (true/false)", raw)
	}
}
