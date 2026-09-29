// Command lua51-lifter decompiles Lua 5.1 bytecode, ported from the Rust
// lua51-lifter binary: it reads one chunk file and writes a sibling
// .dec.51.lua file. Only the Go standard library is used.
package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	deser "github.com/kiet1308/tovek-go/lua51deser"
	"github.com/kiet1308/tovek-go/lua51lifter"
)

const version = "2.1.1"

func main() {
	os.Exit(run(os.Args[1:]))
}

func run(argv []string) int {
	var file string
	for i := 0; i < len(argv); i++ {
		a := argv[i]
		switch {
		case a == "-f" || a == "--file":
			i++
			if i >= len(argv) {
				fmt.Fprintln(os.Stderr, "error: --file requires a value")
				return 2
			}
			file = argv[i]
		case strings.HasPrefix(a, "--file="):
			file = strings.TrimPrefix(a, "--file=")
		case a == "--help" || a == "-h":
			fmt.Print("lua51-lifter: Lua 5.1 bytecode decompiler (Go port)\n\nUsage:\n  lua51-lifter -f <file>\n")
			return 0
		case a == "--version" || a == "-V":
			fmt.Printf("lua51-lifter %s\n", version)
			return 0
		default:
			fmt.Fprintf(os.Stderr, "error: unexpected argument: %s\n", a)
			return 2
		}
	}
	if file == "" {
		fmt.Fprintln(os.Stderr, "error: expected -f <file>")
		return 2
	}
	buffer, err := os.ReadFile(file)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: failed to read file: %s\n", err)
		return 1
	}
	start := time.Now()
	chunk, err := deser.ParseChunk(buffer)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: parse: %s\n", err)
		return 1
	}
	res, err := lua51lifter.DecompileChunk(chunk)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: decompile: %s\n", err)
		return 1
	}
	outName := siblingDecName(file)
	var sb strings.Builder
	fmt.Fprintf(&sb, "-- decompiled by tovek-go (took %s)\n", time.Since(start).Round(time.Millisecond))
	sb.WriteString(res)
	if err := os.WriteFile(outName, []byte(sb.String()), 0o644); err != nil {
		fmt.Fprintf(os.Stderr, "error: write: %s\n", err)
		return 1
	}
	return 0
}

func siblingDecName(file string) string {
	base := filepath.Base(file)
	ext := filepath.Ext(base)
	stem := strings.TrimSuffix(base, ext)
	return filepath.Join(filepath.Dir(file), stem+".dec.51.lua")
}
