package worker

import (
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"testing"
)

func TestAuthorize(t *testing.T) {
	if err := Authorize("s", ""); err != ErrNotConfigured {
		t.Fatalf("empty secret = %v", err)
	}
	if err := Authorize("nope", "s"); err != ErrForbidden {
		t.Fatalf("wrong secret = %v", err)
	}
	if err := Authorize("s", "s"); err != nil {
		t.Fatalf("match = %v", err)
	}
}

func TestDecompileOne(t *testing.T) {
	decode := func(bc []byte, name string) (string, error) {
		if string(bc) == "boom" {
			return "", errors.New("kaboom")
		}
		return fmt.Sprintf("-- %s:%d", name, len(bc)), nil
	}
	msg := fmt.Sprintf(`{"id":"7","encoded_bytecode":%q,"scriptName":"s","dontReuseVar":true}`,
		base64.StdEncoding.EncodeToString([]byte("hi")))
	out, err := DecompileOne([]byte(msg), decode)
	if err != nil {
		t.Fatalf("one: %v", err)
	}
	var resp DecompileResponse
	if err := json.Unmarshal(out, &resp); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if resp.ID != "7" || resp.Decompilation != "-- s:2" {
		t.Fatalf("resp = %+v", resp)
	}
	fail, err := DecompileOne([]byte(fmt.Sprintf(`{"id":"8","encoded_bytecode":%q}`,
		base64.StdEncoding.EncodeToString([]byte("boom")))), decode)
	if err != nil {
		t.Fatalf("fail one: %v", err)
	}
	if !strings.Contains(string(fail), "kaboom") {
		t.Fatalf("fail = %s", fail)
	}
	bad, err := DecompileOne([]byte(`{"id":"9","encoded_bytecode":"!!!"}`), decode)
	if err != nil {
		t.Fatalf("bad b64: %v", err)
	}
	if !strings.Contains(string(bad), "base64") {
		t.Fatalf("bad = %s", bad)
	}
	if _, err := DecompileOne([]byte("{nope"), decode); err == nil {
		t.Fatal("malformed message should fail")
	}
}

func TestDecompileBatchJSON(t *testing.T) {
	calls := 0
	decode := func(bc []byte, ctx BatchItemContext) (string, error) {
		calls++
		if string(bc) == "bad" {
			return "", errors.New("nope")
		}
		name := ""
		if ctx.ScriptName != nil {
			name = *ctx.ScriptName
		}
		return fmt.Sprintf("%s=%d", name, len(bc)), nil
	}
	enc := func(s string) string { return base64.StdEncoding.EncodeToString([]byte(s)) }
	body := fmt.Sprintf(`{"key":203,"scripts":[{},{ "id":"b","encoded_bytecode":%q,"scriptName":"n"},{"id":"c","encoded_bytecode":%q},{"id":"d","encoded_bytecode":"!!!"},{"id":"e","encoded_bytecode":%q,"script_name":"n"}]}`,
		enc("ok"), enc("bad"), enc("ok"))
	out, err := DecompileBatchJSON([]byte(body), 1, decode)
	if err != nil {
		t.Fatalf("batch: %v", err)
	}
	var resp BatchResponse
	if err := json.Unmarshal(out, &resp); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if resp.Count != 5 || resp.OkCount != 2 {
		t.Fatalf("resp = %+v", resp)
	}
	if resp.Results[0].ID != "0" || resp.Results[0].OK {
		t.Fatalf("item 0 = %+v", resp.Results[0])
	}
	if resp.Results[1].ID != "b" || !resp.Results[1].OK {
		t.Fatalf("item 1 = %+v", resp.Results[1])
	}
	if resp.Results[2].OK || resp.Results[2].Error == nil {
		t.Fatalf("item 2 = %+v", resp.Results[2])
	}
	if resp.Results[3].OK {
		t.Fatalf("item 3 = %+v", resp.Results[3])
	}
	if !resp.Results[4].OK {
		t.Fatalf("item 4 = %+v", resp.Results[4])
	}
	// Decode runs only for the two valid-base64 unique entries: "ok" (item
	// b; item e dedups to it) and "bad" (item c). The missing/!b64 items
	// never reach decode.
	if calls != 2 {
		t.Fatalf("dedup should skip the repeated entry, calls = %d", calls)
	}
	if _, err := DecompileBatchJSON([]byte("{nope"), 1, decode); err == nil {
		t.Fatal("malformed batch should fail")
	}
	if _, err := DecompileBatchJSON([]byte(`{"flags":"bogus","scripts":[]}`), 1, decode); err == nil {
		t.Fatal("bad flags should fail")
	}
}
