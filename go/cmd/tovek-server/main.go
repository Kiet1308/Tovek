// Command tovek-server runs the HTTP decompiler service, ported from the
// Rust web-server binary (axum + tokio). It listens on 127.0.0.1:3000 and
// honors TOVEK_UPLOAD_TIMEOUT_SECS, TOVEK_QUEUE_LIMIT and
// TOVEK_QUEUE_TIMEOUT_SECS. Only the Go standard library is used.
package main

import (
	"fmt"
	"net/http"
	"os"
	"strconv"
	"time"

	"github.com/kiet1308/tovek-go/server"
)

const bindAddr = "127.0.0.1:3000"

func main() {
	os.Exit(run())
}

func run() int {
	uploadTimeout, err := secondsEnv("TOVEK_UPLOAD_TIMEOUT_SECS", 30*time.Second)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %s\n", err)
		return 1
	}
	queueLimit, err := uintEnv("TOVEK_QUEUE_LIMIT", 1024)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %s\n", err)
		return 1
	}
	queueTimeout, err := secondsEnv("TOVEK_QUEUE_TIMEOUT_SECS", 120*time.Second)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %s\n", err)
		return 1
	}
	server.QueueLimit = queueLimit
	server.QueueTimeout = queueTimeout
	srv := &http.Server{
		Addr:              bindAddr,
		Handler:           server.NewMux(server.DefaultMaxJobs),
		ReadHeaderTimeout: 5 * time.Second,
		ReadTimeout:       uploadTimeout,
	}
	fmt.Fprintf(os.Stderr, "tovek-server listening on %s\n", bindAddr)
	if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		fmt.Fprintf(os.Stderr, "error: %s\n", err)
		return 1
	}
	return 0
}

func secondsEnv(name string, def time.Duration) (time.Duration, error) {
	v, ok := os.LookupEnv(name)
	if !ok {
		return def, nil
	}
	n, err := strconv.Atoi(v)
	if err != nil || n <= 0 {
		return 0, fmt.Errorf("%s must be a positive 32-bit integer number of seconds", name)
	}
	return time.Duration(n) * time.Second, nil
}

func uintEnv(name string, def int) (int, error) {
	v, ok := os.LookupEnv(name)
	if !ok {
		return def, nil
	}
	n, err := strconv.Atoi(v)
	if err != nil || n < 0 {
		return 0, fmt.Errorf("%s must be a non-negative 32-bit integer", name)
	}
	return n, nil
}
