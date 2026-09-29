// Package server implements the HTTP decompiler service ported from the Rust
// web-server: single-script routes plus a JSON/MDB1 batch route, with bounded
// concurrency (semaphore plus FIFO queue) and per-item batch errors.
//
// Only the Go standard library is used.
package server

import (
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"time"

	"github.com/kiet1308/tovek-go/luau"
)

// Options carries the decompiler flags accepted by every route (headers,
// JSON body, or the MDB1 flag byte).
type Options = luau.DecompileOptions

const (
	// DefaultKey is the Roblox client bytecode decode key.
	DefaultKey byte = 203
	// DefaultMaxJobs caps concurrent decompile jobs when NewMux gets <= 0.
	DefaultMaxJobs = 4

	// MDB1 framing limits, matching the Rust web-server.
	MaxEntries = 50000
	MaxNameLen = 4 * 1024
	MaxCodeLen = 16 * 1024 * 1024

	legacyBodyLimit int64 = 2 * 1024 * 1024
	rawBodyLimit    int64 = 16 * 1024 * 1024
	batchBodyLimit  int64 = 64 * 1024 * 1024
)

// Queue admission tuning. cmd/tovek-server wires these to TOVEK_QUEUE_LIMIT
// and TOVEK_QUEUE_TIMEOUT_SECS; tests may shrink them.
var (
	QueueLimit   = 1024
	QueueTimeout = 120 * time.Second
)

// ParseFlagsText parses "" (defaults), decimal flag bits, or a token list of
// NONE|DONT_REUSE_VAR|STRICT_NO_SYNTHETIC_CONTROL (case-insensitive, '-'
// interchangeable with '_', separated by ',', ';' or whitespace).
func ParseFlagsText(s string) (Options, error) {
	return luau.ParseFlagsText(s)
}

// ParseBool parses 1/true/yes/on as true and 0/false/no/off as false.
func ParseBool(s string) (bool, error) {
	return luau.ParseBool(s)
}

// ---------------------------------------------------------------------------
// MDB1 binary-batch framing
// ---------------------------------------------------------------------------

const (
	mdb1Magic   = "MDB1"
	mdb1Version = 1

	mdb1FlagDontReuseVar             = 1 << 0
	mdb1FlagStrictNoSyntheticControl = 1 << 3
	mdb1SupportedFlags               = mdb1FlagDontReuseVar | mdb1FlagStrictNoSyntheticControl
)

// NamedBytecode is one MDB1 entry: a script name plus raw bytecode.
type NamedBytecode struct {
	Name string
	Code []byte
}

// EncodeMDB1 serializes entries into the MDB1 framing:
// header `MDB1`(4) | version u8 | key u8 | flags u8 | reserved u8(0) |
// count u32 LE, then per entry name_len u32 LE | name | code_len u32 LE | code.
func EncodeMDB1(key byte, opts Options, entries []NamedBytecode) []byte {
	var flags byte
	if opts.DontReuseVar {
		flags |= mdb1FlagDontReuseVar
	}
	if opts.StrictNoSyntheticControl {
		flags |= mdb1FlagStrictNoSyntheticControl
	}
	out := make([]byte, 0, 12)
	out = append(out, 'M', 'D', 'B', '1', mdb1Version, key, flags, 0)
	var n [4]byte
	binary.LittleEndian.PutUint32(n[:], uint32(len(entries)))
	out = append(out, n[:]...)
	for _, e := range entries {
		binary.LittleEndian.PutUint32(n[:], uint32(len(e.Name)))
		out = append(out, n[:]...)
		out = append(out, e.Name...)
		binary.LittleEndian.PutUint32(n[:], uint32(len(e.Code)))
		out = append(out, n[:]...)
		out = append(out, e.Code...)
	}
	return out
}

// ParseMDB1 parses the binary MDB1 raw-batch framing. Every length is
// bounds-checked before slicing, so a hostile or truncated body can never
// panic or read out of bounds.
func ParseMDB1(body []byte) (key byte, opts Options, entries []NamedBytecode, err error) {
	pos := 0
	take := func(n int) ([]byte, bool) {
		if n < 0 || pos+n > len(body) {
			return nil, false
		}
		s := body[pos : pos+n]
		pos += n
		return s, true
	}
	readU32 := func() (uint32, bool) {
		s, ok := take(4)
		if !ok {
			return 0, false
		}
		return binary.LittleEndian.Uint32(s), true
	}
	fail := func(format string, args ...any) (byte, Options, []NamedBytecode, error) {
		return 0, Options{}, nil, fmt.Errorf(format, args...)
	}

	header, ok := take(8)
	if !ok {
		return fail("MDB1: truncated header")
	}
	if string(header[0:4]) != mdb1Magic {
		return fail("MDB1: bad magic (expected an MDB1 batch body; send Content-Type: application/json for a JSON batch)")
	}
	if header[4] != mdb1Version {
		return fail("MDB1: unsupported version %d (this server speaks %d)", header[4], mdb1Version)
	}
	key = header[5]
	flags := header[6]
	if flags&^mdb1SupportedFlags != 0 {
		return fail("MDB1: unsupported flags byte 0x%02X", flags)
	}
	if header[7] != 0 {
		return fail("MDB1: reserved byte must be zero in v1")
	}
	opts = luau.OptionsFromBits(uint32(flags))
	count, ok := readU32()
	if !ok {
		return fail("MDB1: truncated (count)")
	}
	if count > MaxEntries {
		return fail("MDB1: too many entries %d (max %d)", count, MaxEntries)
	}
	entries = make([]NamedBytecode, 0, min(int(count), 1024))
	for i := uint32(0); i < count; i++ {
		nameLen, ok := readU32()
		if !ok {
			return fail("MDB1: truncated (name length)")
		}
		if nameLen > MaxNameLen {
			return fail("MDB1: name too large %d (max %d)", nameLen, MaxNameLen)
		}
		name, ok := take(int(nameLen))
		if !ok {
			return fail("MDB1: truncated (name)")
		}
		codeLen, ok := readU32()
		if !ok {
			return fail("MDB1: truncated (code length)")
		}
		if codeLen > MaxCodeLen {
			return fail("MDB1: code too large %d (max %d)", codeLen, MaxCodeLen)
		}
		code, ok := take(int(codeLen))
		if !ok {
			return fail("MDB1: truncated (code)")
		}
		cp := make([]byte, len(code))
		copy(cp, code)
		entries = append(entries, NamedBytecode{Name: string(name), Code: cp})
	}
	if pos != len(body) {
		return fail("MDB1: %d trailing byte(s) after %d entries", len(body)-pos, count)
	}
	return key, opts, entries, nil
}

// ---------------------------------------------------------------------------
// JSON batch schemas
// ---------------------------------------------------------------------------

// BatchRequest is one JSON batch decompile request body:
// {key?, flags?, dontReuseVar?, scripts:[{id?, bytecode, script_name?}]}.
// The snake_case spellings dont_reuse_var / scriptName are accepted too.
type BatchRequest struct {
	Key          *byte
	Flags        *string
	DontReuseVar bool
	Scripts      []BatchScript
}

// UnmarshalJSON accepts both dontReuseVar and dont_reuse_var.
func (r *BatchRequest) UnmarshalJSON(data []byte) error {
	var aux struct {
		Key             *byte         `json:"key"`
		Flags           *string       `json:"flags"`
		DontReuseVar    *bool         `json:"dontReuseVar"`
		DontReuseVarAlt *bool         `json:"dont_reuse_var"`
		Scripts         []BatchScript `json:"scripts"`
	}
	if err := json.Unmarshal(data, &aux); err != nil {
		return err
	}
	r.Key = aux.Key
	r.Flags = aux.Flags
	if aux.DontReuseVar != nil {
		r.DontReuseVar = *aux.DontReuseVar
	} else if aux.DontReuseVarAlt != nil {
		r.DontReuseVar = *aux.DontReuseVarAlt
	}
	r.Scripts = aux.Scripts
	return nil
}

// BatchScript is one script in a JSON batch request: an optional correlation
// id, base64 bytecode, and an optional script name.
type BatchScript struct {
	ID         *string
	Bytecode   string
	ScriptName *string
}

// UnmarshalJSON accepts both script_name and scriptName.
func (s *BatchScript) UnmarshalJSON(data []byte) error {
	var aux struct {
		ID            *string `json:"id"`
		Bytecode      string  `json:"bytecode"`
		ScriptName    *string `json:"script_name"`
		ScriptNameAlt *string `json:"scriptName"`
	}
	if err := json.Unmarshal(data, &aux); err != nil {
		return err
	}
	s.ID = aux.ID
	s.Bytecode = aux.Bytecode
	s.ScriptName = aux.ScriptName
	if s.ScriptName == nil {
		s.ScriptName = aux.ScriptNameAlt
	}
	return nil
}

// BatchResponse is the JSON batch result:
// {count, ok_count, results:[{index,id?,script_name?,ok,decompilation?,error?}]}.
type BatchResponse struct {
	Count   int               `json:"count"`
	OkCount int               `json:"ok_count"`
	Results []BatchResultItem `json:"results"`
}

// BatchResultItem is the per-script outcome at a zero-based input position.
type BatchResultItem struct {
	Index         int     `json:"index"`
	ID            *string `json:"id,omitempty"`
	ScriptName    *string `json:"script_name,omitempty"`
	OK            bool    `json:"ok"`
	Decompilation *string `json:"decompilation,omitempty"`
	Error         *string `json:"error,omitempty"`
}

// ---------------------------------------------------------------------------
// HTTP mux
// ---------------------------------------------------------------------------

type admission struct {
	slots chan struct{}
	queue chan struct{}
}

// NewMux builds the decompiler routes with at most maxJobs concurrent jobs
// (default 4 when maxJobs <= 0). Requests beyond capacity wait in a bounded
// FIFO queue; a full queue or an expired wait is answered 503 + Retry-After.
func NewMux(maxJobs int) http.Handler {
	if maxJobs <= 0 {
		maxJobs = DefaultMaxJobs
	}
	ql := QueueLimit
	if ql < 0 {
		ql = 0
	}
	a := &admission{
		slots: make(chan struct{}, maxJobs),
		queue: make(chan struct{}, ql),
	}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /decompile", func(w http.ResponseWriter, r *http.Request) {
		a.serve(w, r, legacyBodyLimit, handleDecompile)
	})
	mux.HandleFunc("POST /decompile/raw", func(w http.ResponseWriter, r *http.Request) {
		a.serve(w, r, rawBodyLimit, handleDecompileRaw)
	})
	mux.HandleFunc("POST /decompile/batch", func(w http.ResponseWriter, r *http.Request) {
		a.serve(w, r, batchBodyLimit, handleDecompileBatch)
	})
	return mux
}

func (a *admission) serve(w http.ResponseWriter, r *http.Request, limit int64, h func(http.ResponseWriter, *http.Request, []byte)) {
	select {
	case a.queue <- struct{}{}:
		defer func() { <-a.queue }()
	default:
		reject(w)
		return
	}
	timer := time.NewTimer(QueueTimeout)
	defer timer.Stop()
	select {
	case a.slots <- struct{}{}:
		defer func() { <-a.slots }()
	case <-timer.C:
		reject(w)
		return
	}
	body, ok := readBody(w, r, limit)
	if !ok {
		return
	}
	h(w, r, body)
}

func reject(w http.ResponseWriter) {
	w.Header().Set("Retry-After", "1")
	http.Error(w, "server busy", http.StatusServiceUnavailable)
}

func readBody(w http.ResponseWriter, r *http.Request, limit int64) ([]byte, bool) {
	defer r.Body.Close()
	data, err := io.ReadAll(io.LimitReader(r.Body, limit+1))
	if err != nil {
		http.Error(w, fmt.Sprintf("bad request: %s", err), http.StatusBadRequest)
		return nil, false
	}
	if int64(len(data)) > limit {
		http.Error(w, "body too large", http.StatusRequestEntityTooLarge)
		return nil, false
	}
	return data, true
}

// handleDecompile serves POST /decompile: one script, base64 body, key fixed.
func handleDecompile(w http.ResponseWriter, r *http.Request, body []byte) {
	opts, err := optionsFromHeaders(r)
	if err != nil {
		writeBadRequest(w, err)
		return
	}
	bc, err := base64.StdEncoding.DecodeString(string(body))
	if err != nil {
		writeBadRequest(w, fmt.Errorf("invalid base64 data received: %s", err))
		return
	}
	src, derr := luau.TryDecompileBytecode(bc, DefaultKey, scriptNameFromHeaders(r), opts)
	if derr != nil {
		writeBadRequest(w, derr)
		return
	}
	writeText(w, src)
}

// handleDecompileRaw serves POST /decompile/raw: one script, raw body.
func handleDecompileRaw(w http.ResponseWriter, r *http.Request, body []byte) {
	opts, err := optionsFromHeaders(r)
	if err != nil {
		writeBadRequest(w, err)
		return
	}
	key, err := keyFromHeader(r)
	if err != nil {
		writeBadRequest(w, err)
		return
	}
	src, derr := luau.TryDecompileBytecode(body, key, scriptNameFromHeaders(r), opts)
	if derr != nil {
		writeBadRequest(w, derr)
		return
	}
	writeText(w, src)
}

// handleDecompileBatch serves POST /decompile/batch: JSON when Content-Type
// is application/json, otherwise the binary MDB1 framing. It always answers
// 200 with per-item results; only malformed framing is a 400.
func handleDecompileBatch(w http.ResponseWriter, r *http.Request, body []byte) {
	headerOpts, err := optionsFromHeaders(r)
	if err != nil {
		writeBadRequest(w, err)
		return
	}

	type slot struct {
		id, name *string
		bc       []byte
		key      byte
		perr     *string
	}
	var slots []slot
	var opts Options

	if isJSONContentType(r) {
		var req BatchRequest
		if err := json.Unmarshal(body, &req); err != nil {
			writeBadRequest(w, fmt.Errorf("invalid JSON batch: %s", err))
			return
		}
		if len(req.Scripts) > MaxEntries {
			writeBadRequest(w, fmt.Errorf("too many scripts: %d (max %d)", len(req.Scripts), MaxEntries))
			return
		}
		key := DefaultKey
		if req.Key != nil {
			key = *req.Key
		}
		bodyOpts, err := jsonOptions(req.Flags, req.DontReuseVar)
		if err != nil {
			writeBadRequest(w, err)
			return
		}
		opts = headerOpts.Union(bodyOpts)
		slots = make([]slot, len(req.Scripts))
		for i, item := range req.Scripts {
			// Bad base64 is bad data for one script, not a malformed
			// request: defer it as a per-item failure.
			bc, derr := base64.StdEncoding.DecodeString(item.Bytecode)
			if derr != nil {
				msg := fmt.Sprintf("base64: %s", derr)
				slots[i] = slot{id: item.ID, name: item.ScriptName, perr: &msg}
				continue
			}
			slots[i] = slot{id: item.ID, name: item.ScriptName, bc: bc, key: key}
		}
	} else {
		key, mdbOpts, entries, err := ParseMDB1(body)
		if err != nil {
			writeBadRequest(w, err)
			return
		}
		opts = headerOpts.Union(mdbOpts)
		slots = make([]slot, len(entries))
		for i, e := range entries {
			var name *string
			if e.Name != "" {
				n := e.Name
				name = &n
			}
			slots[i] = slot{name: name, bc: e.Code, key: key}
		}
	}

	inputs := make([]luau.BatchInput, 0, len(slots))
	order := make([]int, 0, len(slots))
	for i, s := range slots {
		if s.perr != nil {
			continue
		}
		inputs = append(inputs, luau.BatchInput{Bytecode: s.bc, EncodeKey: s.key, ScriptName: s.name})
		order = append(order, i)
	}
	outcomes := luau.DecompileBatch(inputs, opts)

	results := make([]BatchResultItem, len(slots))
	for i, s := range slots {
		results[i] = BatchResultItem{Index: i, ID: s.id, ScriptName: s.name}
		if s.perr != nil {
			results[i].Error = s.perr
			continue
		}
	}
	for j, idx := range order {
		if outcomes[j].OK {
			src := outcomes[j].Source
			results[idx].OK = true
			results[idx].Decompilation = &src
		} else {
			msg := outcomes[j].Err
			results[idx].Error = &msg
		}
	}
	okCount := 0
	for _, res := range results {
		if res.OK {
			okCount++
		}
	}
	resp := BatchResponse{Count: len(results), OkCount: okCount, Results: results}
	data, err := json.Marshal(resp)
	if err != nil {
		http.Error(w, "failed to encode response", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(data)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

func writeBadRequest(w http.ResponseWriter, err error) {
	http.Error(w, fmt.Sprintf("bad request: %s", err), http.StatusBadRequest)
}

func writeText(w http.ResponseWriter, s string) {
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.WriteHeader(http.StatusOK)
	_, _ = io.WriteString(w, s)
}

func scriptNameFromHeaders(r *http.Request) *string {
	if v := r.Header.Get("x-script-name"); v != "" {
		return &v
	}
	return nil
}

func optionsFromHeaders(r *http.Request) (Options, error) {
	var opts Options
	if v := r.Header.Get("x-decompile-flags"); v != "" {
		o, err := ParseFlagsText(v)
		if err != nil {
			return opts, err
		}
		opts = opts.Union(o)
	}
	if v := r.Header.Get("x-dont-reuse-var"); v != "" {
		b, err := ParseBool(v)
		if err != nil {
			return opts, fmt.Errorf("x-dont-reuse-var must be a boolean (true/false)")
		}
		if b {
			opts.DontReuseVar = true
		}
	}
	return opts, nil
}

func jsonOptions(flags *string, dontReuseVar bool) (Options, error) {
	var opts Options
	if flags != nil {
		o, err := ParseFlagsText(*flags)
		if err != nil {
			return opts, err
		}
		opts = o
	}
	if dontReuseVar {
		opts.DontReuseVar = true
	}
	return opts, nil
}

func keyFromHeader(r *http.Request) (byte, error) {
	v := r.Header.Get("x-encode-key")
	if v == "" {
		return DefaultKey, nil
	}
	n, err := strconv.Atoi(strings.TrimSpace(v))
	if err != nil || n < 0 || n > 255 {
		return 0, fmt.Errorf("x-encode-key must be 0-255")
	}
	return byte(n), nil
}

func isJSONContentType(r *http.Request) bool {
	ct := r.Header.Get("Content-Type")
	if i := strings.IndexByte(ct, ';'); i >= 0 {
		ct = ct[:i]
	}
	return strings.EqualFold(strings.TrimSpace(ct), "application/json")
}
