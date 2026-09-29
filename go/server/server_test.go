package server

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/kiet1308/tovek-go/luau"
)

func leb128Enc(v uint64) []byte {
	var out []byte
	for {
		b := byte(v & 0x7f)
		v >>= 7
		if v != 0 {
			b |= 0x80
		}
		out = append(out, b)
		if v == 0 {
			return out
		}
	}
}

// encodeOp finds the file byte that decodes to op under key, i.e.
// enc*key%256 == op (brute-forced; the key is always odd so it exists).
func encodeOp(op luau.Opcode, key byte) byte {
	for enc := 0; enc < 256; enc++ {
		if byte(uint32(enc)*uint32(key)%256) == byte(op) {
			return byte(enc)
		}
	}
	panic("no encoding")
}

// minimalChunk builds a tiny v6 chunk: one proto, NOP then RETURN with no
// values, with opcodes encoded for key.
func minimalChunkKey(key byte) []byte {
	var out []byte
	out = append(out, 6, 0)
	out = append(out, leb128Enc(0)...)
	out = append(out, leb128Enc(1)...)
	out = append(out, 1, 0, 0, 0, 0)
	out = append(out, leb128Enc(0)...)
	out = append(out, leb128Enc(2)...)
	out = append(out, encodeOp(luau.OpNOP, key), 0, 0, 0)
	out = append(out, encodeOp(luau.OpRETURN, key), 0, 1, 0)
	out = append(out, leb128Enc(0)...)
	out = append(out, leb128Enc(0)...)
	out = append(out, leb128Enc(0)...)
	out = append(out, leb128Enc(0)...)
	out = append(out, 0, 0)
	out = append(out, leb128Enc(0)...)
	return out
}

func minimalChunk() []byte { return minimalChunkKey(1) }

func TestMDB1Roundtrip(t *testing.T) {
	entries := []NamedBytecode{{Name: "a", Code: []byte{1, 2}}, {Name: "", Code: nil}}
	raw := EncodeMDB1(203, Options{DontReuseVar: true}, entries)
	key, opts, got, err := ParseMDB1(raw)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if key != 203 || !opts.DontReuseVar || opts.StrictNoSyntheticControl {
		t.Fatalf("opts = %+v key %d", opts, key)
	}
	if len(got) != 2 || got[0].Name != "a" || string(got[0].Code) != "\x01\x02" || got[1].Name != "" {
		t.Fatalf("entries = %+v", got)
	}
}

func TestMDB1Rejects(t *testing.T) {
	good := EncodeMDB1(1, Options{}, []NamedBytecode{{Name: "x", Code: []byte{9}}})
	for name, mutate := range map[string]func([]byte) []byte{
		"truncated":   func(b []byte) []byte { return b[:5] },
		"bad magic":   func(b []byte) []byte { c := append([]byte(nil), b...); c[0] = 'X'; return c },
		"bad version": func(b []byte) []byte { c := append([]byte(nil), b...); c[4] = 9; return c },
		"bad flags":   func(b []byte) []byte { c := append([]byte(nil), b...); c[6] = 0xFF; return c },
		"bad reserve": func(b []byte) []byte { c := append([]byte(nil), b...); c[7] = 1; return c },
		"trailing":    func(b []byte) []byte { return append(append([]byte(nil), b...), 0) },
	} {
		if _, _, _, err := ParseMDB1(mutate(good)); err == nil {
			t.Fatalf("%s should fail", name)
		}
	}
}

func TestParseFlagsAndBool(t *testing.T) {
	if o, err := ParseFlagsText(""); err != nil || o != (Options{}) {
		t.Fatalf("empty = %+v %v", o, err)
	}
	o, err := ParseFlagsText("dont-reuse-var, STRICT_NO_SYNTHETIC_CONTROL")
	if err != nil || !o.DontReuseVar || !o.StrictNoSyntheticControl {
		t.Fatalf("tokens = %+v %v", o, err)
	}
	if _, err := ParseFlagsText("bogus"); err == nil {
		t.Fatal("bogus flag should fail")
	}
	if o, err := ParseFlagsText("9"); err != nil || !o.DontReuseVar || !o.StrictNoSyntheticControl {
		t.Fatalf("bits 9 = %+v %v", o, err)
	}
	if _, err := ParseFlagsText("128"); err == nil {
		t.Fatal("unknown bits should fail")
	}
	for s, want := range map[string]bool{"1": true, "yes": true, "ON": true, "0": false, "off": false} {
		if got, err := ParseBool(s); err != nil || got != want {
			t.Fatalf("bool %q = %v %v", s, got, err)
		}
	}
	if _, err := ParseBool("maybe"); err == nil {
		t.Fatal("bad bool should fail")
	}
}

func TestRoutes(t *testing.T) {
	// The legacy /decompile route hardcodes the Roblox client key, so the
	// fixture must be encoded with 203; the raw route honors the header.
	bc203 := minimalChunkKey(203)
	b64 := base64.StdEncoding.EncodeToString(bc203)
	bc := minimalChunk()
	mux := NewMux(4)

	t.Run("legacy base64", func(t *testing.T) {
		req := httptest.NewRequest("POST", "/decompile", strings.NewReader(b64))
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, req)
		if rec.Code != 200 || !strings.Contains(rec.Body.String(), "return") {
			t.Fatalf("code %d body %q", rec.Code, rec.Body.String())
		}
		if ct := rec.Header().Get("Content-Type"); !strings.HasPrefix(ct, "text/plain") {
			t.Fatalf("content-type %q", ct)
		}
	})

	t.Run("legacy bad base64 is 400", func(t *testing.T) {
		req := httptest.NewRequest("POST", "/decompile", strings.NewReader("!!!"))
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, req)
		if rec.Code != http.StatusBadRequest {
			t.Fatalf("code %d", rec.Code)
		}
	})

	t.Run("raw honors key header", func(t *testing.T) {
		req := httptest.NewRequest("POST", "/decompile/raw", strings.NewReader(string(bc)))
		req.Header.Set("x-encode-key", "1")
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, req)
		if rec.Code != 200 || !strings.Contains(rec.Body.String(), "return") {
			t.Fatalf("code %d body %q", rec.Code, rec.Body.String())
		}
	})

	t.Run("raw bad key is 400", func(t *testing.T) {
		req := httptest.NewRequest("POST", "/decompile/raw", strings.NewReader(string(bc)))
		req.Header.Set("x-encode-key", "banana")
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, req)
		if rec.Code != http.StatusBadRequest {
			t.Fatalf("code %d", rec.Code)
		}
	})

	t.Run("json batch per-item errors", func(t *testing.T) {
		goodB64 := base64.StdEncoding.EncodeToString(minimalChunkKey(1))
		body := fmt.Sprintf(`{"key":1,"scripts":[{"id":"good","bytecode":%q},{"id":"bad","bytecode":"!!!"}]}`,
			goodB64)
		req := httptest.NewRequest("POST", "/decompile/batch", strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, req)
		if rec.Code != 200 {
			t.Fatalf("code %d body %q", rec.Code, rec.Body.String())
		}
		var resp BatchResponse
		if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
			t.Fatalf("decode: %v", err)
		}
		if resp.Count != 2 || resp.OkCount != 1 {
			t.Fatalf("resp = %+v", resp)
		}
		if !resp.Results[0].OK || resp.Results[0].Decompilation == nil {
			t.Fatalf("item 0 = %+v", resp.Results[0])
		}
		if resp.Results[1].OK || resp.Results[1].Error == nil {
			t.Fatalf("item 1 = %+v", resp.Results[1])
		}
	})

	t.Run("json batch malformed is 400", func(t *testing.T) {
		req := httptest.NewRequest("POST", "/decompile/batch", strings.NewReader("{nope"))
		req.Header.Set("Content-Type", "application/json")
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, req)
		if rec.Code != http.StatusBadRequest {
			t.Fatalf("code %d", rec.Code)
		}
	})

	t.Run("mdb1 batch", func(t *testing.T) {
		raw := EncodeMDB1(1, Options{}, []NamedBytecode{{Name: "m", Code: bc}})
		req := httptest.NewRequest("POST", "/decompile/batch", strings.NewReader(string(raw)))
		req.Header.Set("Content-Type", "application/octet-stream")
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, req)
		if rec.Code != 200 {
			t.Fatalf("code %d body %q", rec.Code, rec.Body.String())
		}
		var resp BatchResponse
		if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
			t.Fatalf("decode: %v", err)
		}
		if resp.Count != 1 || resp.OkCount != 1 {
			t.Fatalf("resp = %+v", resp)
		}
	})
}

func TestQueueFullRejects(t *testing.T) {
	oldLimit, oldTimeout := QueueLimit, QueueTimeout
	QueueLimit, QueueTimeout = 0, 0
	defer func() { QueueLimit, QueueTimeout = oldLimit, oldTimeout }()
	mux := NewMux(1)
	req := httptest.NewRequest("POST", "/decompile", strings.NewReader("eA=="))
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, req)
	if rec.Code != http.StatusServiceUnavailable {
		t.Fatalf("code %d", rec.Code)
	}
	if rec.Header().Get("Retry-After") == "" {
		t.Fatal("missing Retry-After")
	}
}
