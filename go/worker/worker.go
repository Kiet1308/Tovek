// Package worker implements the Cloudflare-independent core of the
// luau-worker: message schemas, single/batch decompilation with injected
// decode functions, bounded duplicate reuse, and auth checks.
//
// Only the Go standard library is used.
package worker

import (
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"

	"github.com/kiet1308/tovek-go/luau"
)

// DecompileOptions carries the decompiler flags for one batch or message.
type DecompileOptions = luau.DecompileOptions

var (
	// ErrNotConfigured is returned when no auth secret is configured (503-ish).
	ErrNotConfigured = errors.New("worker: authentication is not configured")
	// ErrForbidden is returned when the credential does not match (403-ish).
	ErrForbidden = errors.New("worker: invalid license")
)

// Authorize checks an Authorization header value against the secret.
func Authorize(headerValue, secret string) error {
	if secret == "" {
		return ErrNotConfigured
	}
	if headerValue != secret {
		return ErrForbidden
	}
	return nil
}

// DecompileMessage is one single-script decompile request:
// {id, encoded_bytecode, script_name?, dont_reuse_var?, flags?}.
// The camelCase spellings scriptName / dontReuseVar are accepted too.
type DecompileMessage struct {
	ID              string
	EncodedBytecode string
	ScriptName      *string
	DontReuseVar    bool
	Flags           *string
}

// UnmarshalJSON accepts both script_name/scriptName and
// dont_reuse_var/dontReuseVar.
func (m *DecompileMessage) UnmarshalJSON(data []byte) error {
	var aux struct {
		ID              string  `json:"id"`
		EncodedBytecode string  `json:"encoded_bytecode"`
		ScriptName      *string `json:"script_name"`
		ScriptNameAlt   *string `json:"scriptName"`
		DontReuseVar    *bool   `json:"dont_reuse_var"`
		DontReuseVarAlt *bool   `json:"dontReuseVar"`
		Flags           *string `json:"flags"`
	}
	if err := json.Unmarshal(data, &aux); err != nil {
		return err
	}
	m.ID = aux.ID
	m.EncodedBytecode = aux.EncodedBytecode
	m.ScriptName = aux.ScriptName
	if m.ScriptName == nil {
		m.ScriptName = aux.ScriptNameAlt
	}
	if aux.DontReuseVar != nil {
		m.DontReuseVar = *aux.DontReuseVar
	} else if aux.DontReuseVarAlt != nil {
		m.DontReuseVar = *aux.DontReuseVarAlt
	}
	m.Flags = aux.Flags
	return nil
}

// DecompileResponse is one single-script result: {id, decompilation}.
type DecompileResponse struct {
	ID            string `json:"id"`
	Decompilation string `json:"decompilation"`
}

// BatchItem is one script in a batch request.
type BatchItem struct {
	ID              *string
	EncodedBytecode *string
	ScriptName      *string
}

// UnmarshalJSON accepts both script_name and scriptName.
func (b *BatchItem) UnmarshalJSON(data []byte) error {
	var aux struct {
		ID            *string `json:"id"`
		Encoded       *string `json:"encoded_bytecode"`
		ScriptName    *string `json:"script_name"`
		ScriptNameAlt *string `json:"scriptName"`
	}
	if err := json.Unmarshal(data, &aux); err != nil {
		return err
	}
	b.ID = aux.ID
	b.EncodedBytecode = aux.Encoded
	b.ScriptName = aux.ScriptName
	if b.ScriptName == nil {
		b.ScriptName = aux.ScriptNameAlt
	}
	return nil
}

// BatchRequest is one batch decompile request body.
// The dontReuseVar camelCase spelling is accepted too.
type BatchRequest struct {
	Key          *byte
	DontReuseVar bool
	Flags        *string
	Scripts      []BatchItem
}

// UnmarshalJSON accepts both dont_reuse_var and dontReuseVar.
func (r *BatchRequest) UnmarshalJSON(data []byte) error {
	var aux struct {
		Key             *byte       `json:"key"`
		DontReuseVar    *bool       `json:"dontReuseVar"`
		DontReuseVarAlt *bool       `json:"dont_reuse_var"`
		Flags           *string     `json:"flags"`
		Scripts         []BatchItem `json:"scripts"`
	}
	if err := json.Unmarshal(data, &aux); err != nil {
		return err
	}
	r.Key = aux.Key
	if aux.DontReuseVar != nil {
		r.DontReuseVar = *aux.DontReuseVar
	} else if aux.DontReuseVarAlt != nil {
		r.DontReuseVar = *aux.DontReuseVarAlt
	}
	r.Flags = aux.Flags
	r.Scripts = aux.Scripts
	return nil
}

// BatchResultItem is the per-script outcome at a zero-based input position.
// A missing id defaults to the decimal index, matching the Rust worker.
type BatchResultItem struct {
	Index         int     `json:"index"`
	ID            string  `json:"id"`
	OK            bool    `json:"ok"`
	Decompilation *string `json:"decompilation,omitempty"`
	Error         *string `json:"error,omitempty"`
}

// BatchResponse is the JSON batch result: {count, ok_count, results}.
type BatchResponse struct {
	Count   int               `json:"count"`
	OkCount int               `json:"ok_count"`
	Results []BatchResultItem `json:"results"`
}

// DecodeFunc decompiles decoded bytecode; scriptName is "" when absent.
type DecodeFunc func(bytecode []byte, scriptName string) (string, error)

// DecompileOne handles one JSON decompile message and returns the
// {"id","decompilation"} response. Decompile failures become a
// "-- decompile failed: ..." value, not an error; only a malformed message
// itself is an error.
func DecompileOne(msgBytes []byte, decode DecodeFunc) ([]byte, error) {
	var msg DecompileMessage
	if err := json.Unmarshal(msgBytes, &msg); err != nil {
		return nil, err
	}
	name := ""
	if msg.ScriptName != nil {
		name = *msg.ScriptName
	}
	var decompilation string
	switch {
	case msg.EncodedBytecode == "":
		decompilation = "-- decompile failed: missing encoded_bytecode"
	default:
		bc, err := base64.StdEncoding.DecodeString(msg.EncodedBytecode)
		if err != nil {
			decompilation = fmt.Sprintf("-- decompile failed: base64: %s", err)
			break
		}
		src, err := decode(bc, name)
		if err != nil {
			decompilation = fmt.Sprintf("-- decompile failed: %s", err)
		} else {
			decompilation = src
		}
	}
	return json.Marshal(DecompileResponse{ID: msg.ID, Decompilation: decompilation})
}

// BatchItemContext describes one script handed to BatchDecodeFunc.
type BatchItemContext struct {
	Key        byte
	ScriptName *string
	Options    DecompileOptions
}

// BatchDecodeFunc decompiles one decoded script under the batch's key/options.
type BatchDecodeFunc func(bytecode []byte, ctx BatchItemContext) (string, error)

const (
	maxDedupEntries  = 1024
	maxDedupKeyBytes = 4 * 1024 * 1024
)

// DecompileBatchJSON handles one JSON batch body and returns the
// {count, ok_count, results} response. One bad script becomes a per-item
// error; only malformed JSON or flags abort the batch with an error.
// Identical (encoded text, script name) inputs share one execution through a
// bounded, request-local dedup table (1024 entries / 4MiB of keys).
func DecompileBatchJSON(body []byte, defaultKey byte, decode BatchDecodeFunc) ([]byte, error) {
	var req BatchRequest
	if err := json.Unmarshal(body, &req); err != nil {
		return nil, fmt.Errorf("invalid JSON batch: %s", err)
	}
	key := defaultKey
	if req.Key != nil {
		key = *req.Key
	}
	var flags string
	if req.Flags != nil {
		flags = *req.Flags
	}
	opts, err := luau.ParseFlagsText(flags)
	if err != nil {
		return nil, err
	}
	if req.DontReuseVar {
		opts.DontReuseVar = true
	}

	type dedupKey struct {
		encoded string
		name    string
		hasName bool
	}
	seen := make(map[dedupKey]int)
	keyBytes := 0
	results := make([]BatchResultItem, 0, len(req.Scripts))
	for i, item := range req.Scripts {
		id := strconv.Itoa(i)
		if item.ID != nil {
			id = *item.ID
		}
		name := ""
		hasName := false
		if item.ScriptName != nil {
			name = *item.ScriptName
			hasName = true
		}
		encoded := ""
		if item.EncodedBytecode != nil {
			encoded = *item.EncodedBytecode
		}
		k := dedupKey{encoded: encoded, name: name, hasName: hasName}
		if prev, ok := seen[k]; ok {
			p := results[prev]
			results = append(results, BatchResultItem{
				Index: i, ID: id, OK: p.OK,
				Decompilation: p.Decompilation, Error: p.Error,
			})
			continue
		}
		var outcome string
		var reason *string
		ok := true
		bc, berr := base64.StdEncoding.DecodeString(encoded)
		switch {
		case item.EncodedBytecode == nil:
			msg := "missing encoded_bytecode"
			reason = &msg
			ok = false
		case berr != nil:
			msg := fmt.Sprintf("base64: %s", berr)
			reason = &msg
			ok = false
		default:
			src, derr := decode(bc, BatchItemContext{Key: key, ScriptName: item.ScriptName, Options: opts})
			if derr != nil {
				msg := derr.Error()
				reason = &msg
				ok = false
			} else {
				outcome = src
			}
		}
		var srcp *string
		if ok {
			srcp = &outcome
		}
		results = append(results, BatchResultItem{
			Index: i, ID: id, OK: ok, Decompilation: srcp, Error: reason,
		})
		if kb := len(encoded) + len(name); len(seen) < maxDedupEntries && kb <= maxDedupKeyBytes-keyBytes {
			seen[k] = len(results) - 1
			keyBytes += kb
		}
	}
	okCount := 0
	for _, res := range results {
		if res.OK {
			okCount++
		}
	}
	return json.Marshal(BatchResponse{Count: len(results), OkCount: okCount, Results: results})
}
