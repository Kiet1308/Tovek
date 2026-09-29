// Command luau-lifter decompiles Luau bytecode, ported from the Rust
// luau-lifter binary: single-file mode plus the decompile-folder and
// validate-folder subcommands. Only the Go standard library is used.
package main

import (
	"encoding/base64"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"

	"github.com/kiet1308/tovek-go/luau"
)

const version = "2.1.1"

func main() {
	os.Exit(run(os.Args[1:]))
}

func run(argv []string) int {
	if len(argv) > 0 {
		switch argv[0] {
		case "decompile-folder":
			return runFolder(argv[1:], false)
		case "validate-folder":
			return runFolder(argv[1:], true)
		case "--help", "-h", "help":
			usage()
			return 0
		case "--version", "-V", "version":
			fmt.Printf("luau-lifter %s\n", version)
			return 0
		}
	}
	return runSingleFile(argv)
}

func usage() {
	fmt.Print(`luau-lifter: Luau bytecode decompiler (Go port)

Usage:
  luau-lifter <file.luac> [-e] [--script-name NAME] [options]
  luau-lifter decompile-folder SRC OUT [options]
  luau-lifter validate-folder SRC OUT [options]

Single-file options:
  -e                              decode with the Roblox client key (203)
  --key N                         decode key 0-255 (default 1; 203 with -e)
  --script-name NAME              module hint for name recovery
  --dont-reuse-var                version registers instead of reusing names
  --no-synth-helpers              skip synthesized helper comments
  --assume-no-nan                 permit NaN-unsafe condition flips
  --synthesize-arithmetic-loops   experimental arithmetic loop synthesis
  --allow-certified-dispatcher    permit the synthetic dispatcher
  --strict-no-synthetic-control   fail closed on unstructured control

Folder options (decompile-folder, validate-folder):
  SRC OUT                         source tree of saved-bytecode .lua files,
                                  output tree with .lua renamed to .luau
  -e, --key N                     decode key (default 203 for folders)
  -t, --threads N                 worker threads (0 = all CPUs)
  -v, --verbose                   print one line per decompiled file
  --dont-reuse-var, --no-synth-helpers, --assume-no-nan,
  --synthesize-arithmetic-loops, --strict-no-synthetic-control,
  --allow-certified-dispatcher    (same meaning as single-file mode)
  --output-extension lua|luau     output extension (default luau)
`)
}

type folderFlags struct {
	src, out  string
	key       byte
	threads   int
	verbose   bool
	opts      luau.DecompileOptions
	extension string
}

func parseFolderFlags(argv []string) (folderFlags, error) {
	f := folderFlags{key: 203, extension: "luau"}
	var positional []string
	i := 0
	for ; i < len(argv); i++ {
		a := argv[i]
		if !strings.HasPrefix(a, "-") || a == "-" {
			positional = append(positional, a)
			continue
		}
		if a == "--" {
			positional = append(positional, argv[i+1:]...)
			break
		}
		name, val, hasVal := splitFlag(a)
		takeVal := func(what string) (string, error) {
			if hasVal {
				return val, nil
			}
			i++
			if i >= len(argv) {
				return "", fmt.Errorf("%s requires a value", what)
			}
			return argv[i], nil
		}
		switch name {
		case "-e", "--encoded":
			f.key = 203
		case "--key":
			v, err := takeVal("--key")
			if err != nil {
				return f, err
			}
			n, err := strconv.Atoi(v)
			if err != nil || n < 0 || n > 255 {
				return f, fmt.Errorf("--key must be 0-255")
			}
			f.key = byte(n)
		case "-t", "--threads":
			v, err := takeVal("--threads")
			if err != nil {
				return f, err
			}
			n, err := strconv.Atoi(v)
			if err != nil || n < 0 {
				return f, fmt.Errorf("--threads must be >= 0")
			}
			f.threads = n
		case "-v", "--verbose":
			f.verbose = true
		case "--dont-reuse-var":
			f.opts.DontReuseVar = true
		case "--no-synth-helpers":
			f.opts.NoSynthHelpers = true
		case "--assume-no-nan":
			f.opts.AssumeNoNaN = true
		case "--synthesize-arithmetic-loops":
			f.opts.SynthesizeArithmeticLoops = true
		case "--strict-no-synthetic-control":
			f.opts.StrictNoSyntheticControl = true
		case "--allow-certified-dispatcher":
			f.opts.StrictNoSyntheticControl = false
		case "--output-extension":
			v, err := takeVal("--output-extension")
			if err != nil {
				return f, err
			}
			if v != "lua" && v != "luau" {
				return f, fmt.Errorf(`--output-extension must be "lua" or "luau"`)
			}
			f.extension = v
		case "--emit-upvalue-analysis", "--emit-binding-provenance",
			"--compact-annotations", "--export-manifest",
			"--cache-dir", "--cache-max-mib", "--analyze", "--tool-dir", "--solver":
			return f, fmt.Errorf("%s is accepted for compatibility but not implemented in the Go port", name)
		default:
			return f, fmt.Errorf("unexpected argument: %s", a)
		}
	}
	if len(positional) != 2 {
		return f, fmt.Errorf("expected SRC OUT, got %d positional arguments", len(positional))
	}
	f.src, f.out = positional[0], positional[1]
	return f, nil
}

func splitFlag(a string) (name, val string, hasVal bool) {
	if i := strings.IndexByte(a, '='); i >= 0 {
		return a[:i], a[i+1:], true
	}
	return a, "", false
}

func runSingleFile(argv []string) int {
	var file string
	key := byte(1)
	var scriptName *string
	var opts luau.DecompileOptions
	i := 0
	for ; i < len(argv); i++ {
		a := argv[i]
		switch {
		case a == "-e":
			key = 203
		case a == "--dont-reuse-var":
			opts.DontReuseVar = true
		case a == "--no-synth-helpers":
			opts.NoSynthHelpers = true
		case a == "--assume-no-nan":
			opts.AssumeNoNaN = true
		case a == "--synthesize-arithmetic-loops":
			opts.SynthesizeArithmeticLoops = true
		case a == "--allow-certified-dispatcher":
			opts.StrictNoSyntheticControl = false
		case a == "--strict-no-synthetic-control":
			opts.StrictNoSyntheticControl = true
		case a == "--script-name":
			i++
			if i >= len(argv) {
				fmt.Fprintln(os.Stderr, "error: --script-name requires a value")
				return 2
			}
			v := argv[i]
			scriptName = &v
		case strings.HasPrefix(a, "--key="):
			n, err := strconv.Atoi(strings.TrimPrefix(a, "--key="))
			if err != nil || n < 0 || n > 255 {
				fmt.Fprintln(os.Stderr, "error: --key must be 0-255")
				return 2
			}
			key = byte(n)
		case a == "--key":
			i++
			if i >= len(argv) {
				fmt.Fprintln(os.Stderr, "error: --key requires a value")
				return 2
			}
			n, err := strconv.Atoi(argv[i])
			if err != nil || n < 0 || n > 255 {
				fmt.Fprintln(os.Stderr, "error: --key must be 0-255")
				return 2
			}
			key = byte(n)
		case strings.HasPrefix(a, "-"):
			fmt.Fprintf(os.Stderr, "error: unexpected argument: %s\n", a)
			return 2
		default:
			if file != "" {
				fmt.Fprintln(os.Stderr, "error: expected exactly one file")
				return 2
			}
			file = a
		}
	}
	if file == "" {
		usage()
		return 2
	}
	bytecode, err := os.ReadFile(file)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: failed to read file: %s\n", err)
		return 1
	}
	src, err := luau.TryDecompileBytecode(bytecode, key, scriptName, opts)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %s\n", err)
		return 1
	}
	fmt.Println(src)
	return 0
}

type workItem struct {
	rel    string
	input  string
	output string
}

func runFolder(argv []string, validate bool) int {
	f, err := parseFolderFlags(argv)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %s\n", err)
		return 2
	}
	var work []workItem
	err = filepath.Walk(f.src, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if info.IsDir() {
			return nil
		}
		if !strings.HasSuffix(strings.ToLower(info.Name()), ".lua") {
			return nil
		}
		rel, err := filepath.Rel(f.src, path)
		if err != nil {
			return err
		}
		relSlash := filepath.ToSlash(rel)
		outRel := strings.TrimSuffix(relSlash, filepath.Ext(relSlash)) + "." + f.extension
		work = append(work, workItem{
			rel:    relSlash,
			input:  path,
			output: filepath.Join(f.out, filepath.FromSlash(outRel)),
		})
		return nil
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %s\n", err)
		return 2
	}
	sort.Slice(work, func(i, j int) bool { return work[i].rel < work[j].rel })
	if len(work) == 0 {
		fmt.Fprintf(os.Stderr, "no .lua files found under %s\n", f.src)
	}
	if validate {
		if _, err := lookPath("luau-analyze"); err != nil {
			fmt.Fprintln(os.Stderr, "error: luau-analyze executable not found (validate-folder needs it on PATH)")
			return 2
		}
	}
	threads := f.threads
	if threads <= 0 {
		threads = runtime.NumCPU()
	}
	if threads < 1 {
		threads = 1
	}
	var okCount, skipCount, failCount int64
	jobs := make(chan workItem)
	var wg sync.WaitGroup
	for w := 0; w < threads; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for item := range jobs {
				if decompileOne(item, f) {
					atomic.AddInt64(&okCount, 1)
				} else {
					atomic.AddInt64(&failCount, 1)
				}
			}
		}()
	}
	for _, item := range work {
		jobs <- item
	}
	close(jobs)
	wg.Wait()
	_ = skipCount
	ok, fail := int(okCount), int(failCount)
	fmt.Fprintf(os.Stderr, "----------------------------------------\n")
	fmt.Fprintf(os.Stderr, "Done: %d decompiled, %d failed.\n", ok, fail)
	fmt.Fprintf(os.Stderr, "Output: %s\n", f.out)
	if fail > 0 {
		return 1
	}
	return 0
}

func lookPath(name string) (string, error) {
	for _, dir := range strings.Split(os.Getenv("PATH"), string(os.PathListSeparator)) {
		if dir == "" {
			continue
		}
		if fi, err := os.Stat(filepath.Join(dir, name)); err == nil && !fi.IsDir() {
			return filepath.Join(dir, name), nil
		}
	}
	return "", fmt.Errorf("%s not found", name)
}

// decompileOne mirrors the Rust decode/decompile/write core for one saved
// bytecode file: it replicates `grep -v '^--' | tr -d ' \t\r\n' | base64 -d`,
// decompiles with the file's rel path as the script name, and mirrors the
// output tree. It reports true on success.
func decompileOne(item workItem, f folderFlags) bool {
	text, err := os.ReadFile(item.input)
	if err != nil {
		fmt.Fprintf(os.Stderr, "FAIL %s\n      read: %s\n", item.rel, err)
		return false
	}
	var b64 []byte
	for _, line := range strings.Split(string(text), "\n") {
		if strings.HasPrefix(line, "--") {
			continue
		}
		for i := 0; i < len(line); i++ {
			if c := line[i]; c != ' ' && c != '\t' && c != '\r' {
				b64 = append(b64, c)
			}
		}
	}
	if len(b64) == 0 {
		return true
	}
	bytecode, err := base64.StdEncoding.DecodeString(string(b64))
	if err != nil {
		fmt.Fprintf(os.Stderr, "FAIL %s\n      base64: %s\n", item.rel, err)
		return false
	}
	name := item.rel
	src, err := luau.TryDecompileBytecode(bytecode, f.key, &name, f.opts)
	if err != nil {
		fmt.Fprintf(os.Stderr, "FAIL %s\n      %s\n", item.rel, err)
		return false
	}
	if err := os.MkdirAll(filepath.Dir(item.output), 0o755); err != nil {
		fmt.Fprintf(os.Stderr, "FAIL %s\n      mkdir: %s\n", item.rel, err)
		return false
	}
	if err := os.WriteFile(item.output, []byte(src), 0o644); err != nil {
		fmt.Fprintf(os.Stderr, "FAIL %s\n      write: %s\n", item.rel, err)
		return false
	}
	if f.verbose {
		fmt.Fprintf(os.Stderr, "ok %s\n", item.rel)
	}
	return true
}
