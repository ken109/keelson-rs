package testutils

// Golden test extraction for keelson.
// Enabled by setting KEELSON_GOLDEN_OUT to a file path; each test case is
// appended as one JSON object per line (JSONL).

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
)

type GoldenArg struct {
	GoType string          `json:"go_type"`
	Repr   string          `json:"repr"`
	JSON   json.RawMessage `json:"json,omitempty"`
}

type GoldenCase struct {
	Kind    string `json:"kind"`    // "query" | "expression"
	Dialect string `json:"dialect"` // psql | mysql | sqlite | ""
	Source  string `json:"source"`  // e.g. dialect/psql/select_test.go
	Name    string `json:"name"`    // testcase key
	Doc     string `json:"doc,omitempty"`

	// SQL as written in bob's testcase (pre-normalisation, may be loosely formatted)
	ExpectedSQL string `json:"expected_sql"`
	// SQL actually produced by bob — this is the real golden value
	GeneratedSQL string `json:"generated_sql"`
	// GeneratedSQL run through testutils.Clean (whitespace/bracket normalisation)
	CleanSQL string `json:"clean_sql"`
	// GeneratedSQL run through the dialect formatter (parse + deparse), when available
	NormalizedSQL string `json:"normalized_sql,omitempty"`
	// Set when the dialect formatter failed on GeneratedSQL
	NormalizeError string `json:"normalize_error,omitempty"`

	Args     []GoldenArg `json:"args"`
	BuildErr string      `json:"build_error,omitempty"`
	// Expression tests only
	ExpectedError string `json:"expected_error,omitempty"`
}

var (
	goldenMu   sync.Mutex
	goldenFile *os.File
)

func goldenOut() *os.File {
	goldenMu.Lock()
	defer goldenMu.Unlock()

	if goldenFile != nil {
		return goldenFile
	}

	path := os.Getenv("KEELSON_GOLDEN_OUT")
	if path == "" {
		return nil
	}

	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		panic(fmt.Sprintf("golden: cannot open %s: %v", path, err))
	}
	goldenFile = f

	return goldenFile
}

func goldenEmit(c GoldenCase) {
	f := goldenOut()
	if f == nil {
		return
	}

	b, err := json.Marshal(c)
	if err != nil {
		panic(fmt.Sprintf("golden: marshal: %v", err))
	}

	goldenMu.Lock()
	defer goldenMu.Unlock()
	if _, err := f.Write(append(b, '\n')); err != nil {
		panic(fmt.Sprintf("golden: write: %v", err))
	}
	// Tests may be killed abruptly; keep the file consistent.
	_ = f.Sync()
}

func goldenArgs(args []any) []GoldenArg {
	out := make([]GoldenArg, 0, len(args))
	for _, a := range args {
		g := GoldenArg{
			GoType: fmt.Sprintf("%T", a),
			Repr:   fmt.Sprintf("%#v", a),
		}
		if b, err := json.Marshal(a); err == nil {
			g.JSON = b
		}
		out = append(out, g)
	}

	return out
}

// goldenSource reports the *_test.go file that invoked the harness, relative to
// the module root, plus the dialect inferred from its directory.
func goldenSource(skip int) (source, dialect string) {
	for i := skip; i < skip+12; i++ {
		_, file, _, ok := runtime.Caller(i)
		if !ok {
			break
		}
		if !strings.HasSuffix(file, "_test.go") {
			continue
		}

		dir, base := filepath.Split(file)
		parts := strings.Split(strings.Trim(dir, string(filepath.Separator)), string(filepath.Separator))

		// find ".../bob/<rest...>/<base>"
		for j := len(parts) - 1; j >= 0; j-- {
			if parts[j] == "bob" || strings.HasPrefix(parts[j], "bob@") {
				source = filepath.Join(append(parts[j+1:], base)...)
				break
			}
		}
		if source == "" {
			source = base
		}

		for _, d := range []string{"psql", "mysql", "sqlite"} {
			for _, p := range parts {
				if p == d {
					dialect = d
				}
			}
		}

		return source, dialect
	}

	return "", ""
}
