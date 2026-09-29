package main

import (
	"encoding/base64"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/kiet1308/tovek-go/luau"
)

// buildPrintHello reuses the same hand-rolled v6 chunk shape as the luau
// e2e test: GETIMPORT print; LOADK "hello"; CALL; RETURN.
func buildPrintHello(t *testing.T, key byte) []byte {
	t.Helper()
	enc := func(op luau.Opcode) byte {
		for e := 0; e < 256; e++ {
			if byte(uint32(e)*uint32(key)%256) == byte(op) {
				return byte(e)
			}
		}
		t.Fatal("no encoding")
		return 0
	}
	uleb := func(v uint64) []byte {
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
	var out []byte
	out = append(out, 6, 0)
	out = append(out, uleb(2)...)
	for _, s := range []string{"print", "hello"} {
		out = append(out, uleb(uint64(len(s)))...)
		out = append(out, s...)
	}
	out = append(out, uleb(1)...)
	var proto []byte
	proto = append(proto, 2, 0, 0, 1, 0)
	proto = append(proto, uleb(0)...)
	var code []byte
	code = append(code, enc(luau.OpGETIMPORT), 0, 0, 0, 0, 0, 0, 1<<6)
	code = append(code, enc(luau.OpLOADK), 1, 1, 0)
	code = append(code, enc(luau.OpCALL), 0, 2, 1)
	code = append(code, enc(luau.OpRETURN), 0, 0, 0)
	proto = append(proto, uleb(uint64(len(code)/4))...)
	proto = append(proto, code...)
	proto = append(proto, uleb(2)...)
	proto = append(proto, 3)
	proto = append(proto, uleb(1)...)
	proto = append(proto, 3)
	proto = append(proto, uleb(2)...)
	proto = append(proto, uleb(0)...)
	proto = append(proto, uleb(0)...)
	proto = append(proto, uleb(0)...)
	proto = append(proto, 0, 0)
	out = append(out, proto...)
	out = append(out, uleb(0)...)
	return out
}

func TestSingleFileRun(t *testing.T) {
	dir := t.TempDir()
	raw := buildPrintHello(t, 1)
	path := filepath.Join(dir, "hello.luac")
	if err := os.WriteFile(path, raw, 0o644); err != nil {
		t.Fatal(err)
	}
	if code := run([]string{path, "--script-name", "hello"}); code != 0 {
		t.Fatalf("exit = %d", code)
	}
	if code := run([]string{path + ".missing"}); code == 0 {
		t.Fatal("missing file should fail")
	}
	if code := run([]string{}); code == 0 {
		t.Fatal("no args should fail")
	}
	if code := run([]string{path, "--bogus"}); code == 0 {
		t.Fatal("bogus flag should fail")
	}
}

func TestDecompileFolderRun(t *testing.T) {
	dir := t.TempDir()
	src := filepath.Join(dir, "dump", "game")
	if err := os.MkdirAll(src, 0o755); err != nil {
		t.Fatal(err)
	}
	payload := base64.StdEncoding.EncodeToString(buildPrintHello(t, 203))
	wrapped := "-- Saved by test\n-- another header\n" + payload + "\n"
	if err := os.WriteFile(filepath.Join(src, "script.lua"), []byte(wrapped), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(src, "notes.txt"), []byte("skip me"), 0o644); err != nil {
		t.Fatal(err)
	}
	out := filepath.Join(dir, "out")
	if code := run([]string{"decompile-folder", filepath.Join(dir, "dump"), out, "-v"}); code != 0 {
		t.Fatalf("exit = %d", code)
	}
	got, err := os.ReadFile(filepath.Join(out, "game", "script.luau"))
	if err != nil {
		t.Fatalf("read output: %v", err)
	}
	if !strings.Contains(string(got), "= print") || !strings.Contains(string(got), `"hello"`) {
		t.Fatalf("output:\n%s", got)
	}
	if code := run([]string{"decompile-folder", filepath.Join(dir, "dump"), out, "--bogus"}); code == 0 {
		t.Fatal("bogus flag should fail")
	}
	if code := run([]string{"decompile-folder", "only-one"}); code == 0 {
		t.Fatal("missing OUT should fail")
	}
}
