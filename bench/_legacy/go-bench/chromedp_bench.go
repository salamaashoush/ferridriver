// chromedp head-to-head bench.
//
// Same shape as bench/fd-bench/bench_compare.spec.ts — 100 tests
// split 33 nav / 33 click / 34 eval against data: URLs. Per-test:
// open fresh page, goto, perform action, verify, close.
//
// Build:  go build -o chromedp-bench chromedp_bench.go
// Args:   --workers N --chrome <path>
// Output: total wall time ms on first stdout line.
//
//go:build chromedp

package main

import (
	"context"
	"flag"
	"fmt"
	"net/url"
	"os"
	"sync"
	"time"

	"github.com/chromedp/chromedp"
)

type kind int

const (
	kNav kind = iota
	kClick
	kEval
)

type tc struct {
	i    int
	kind kind
}

func buildTests(n int) []tc {
	out := make([]tc, n)
	for i := 0; i < n; i++ {
		out[i] = tc{i: i, kind: kind(i % 3)}
	}
	return out
}

func dataURL(html string) string {
	return "data:text/html," + url.PathEscape(html)
}

func runOne(allocCtx context.Context, t tc) error {
	// Fresh per-test context — analogue of Playwright's per-test
	// browser context. chromedp's `chromedp.NewContext` derives a
	// new tab; for fair isolation we use it here.
	ctx, cancel := chromedp.NewContext(allocCtx)
	defer cancel()
	tCtx, tCancel := context.WithTimeout(ctx, 10*time.Second)
	defer tCancel()

	switch t.kind {
	case kNav:
		want := fmt.Sprintf("Test %d", t.i)
		var got string
		if err := chromedp.Run(tCtx,
			chromedp.Navigate(dataURL(fmt.Sprintf("<title>%s</title><body><h1>Page %d</h1></body>", want, t.i))),
			chromedp.Title(&got),
		); err != nil {
			return err
		}
		if got != want {
			return fmt.Errorf("nav: got %q want %q", got, want)
		}
	case kClick:
		want := fmt.Sprintf("done %d", t.i)
		var got string
		if err := chromedp.Run(tCtx,
			chromedp.Navigate(dataURL(fmt.Sprintf(
				"<button id='btn' onclick=\"this.textContent='%s'\">Click %d</button>", want, t.i))),
			chromedp.Click("#btn", chromedp.ByID),
		); err != nil {
			return err
		}
		// Spin-poll text — chromedp's WaitFunc API requires a query option
		// hooked to a Selector, but for this bench we just want a text check.
		// Hand-roll the same loop ferridriver's expect.toHaveText uses.
		deadline := time.Now().Add(5 * time.Second)
		for time.Now().Before(deadline) {
			if err := chromedp.Run(tCtx, chromedp.Text("#btn", &got, chromedp.ByID)); err != nil {
				return err
			}
			if got == want {
				break
			}
			time.Sleep(20 * time.Millisecond)
		}
		if got != want {
			return fmt.Errorf("click: text %q != %q", got, want)
		}
	case kEval:
		want := fmt.Sprintf("%d", t.i)
		var got string
		if err := chromedp.Run(tCtx,
			chromedp.Navigate(dataURL(fmt.Sprintf("<title>Eval %d</title><div id='out'>%d</div>", t.i, t.i))),
			chromedp.Evaluate(`document.getElementById('out')?.textContent`, &got),
		); err != nil {
			return err
		}
		if got != want {
			return fmt.Errorf("eval: got %q want %q", got, want)
		}
	}
	return nil
}

func main() {
	workers := flag.Int("workers", 1, "concurrent workers")
	chromePath := flag.String("chrome", "", "chrome executable path")
	flag.Parse()

	opts := append(chromedp.DefaultExecAllocatorOptions[:],
		chromedp.Headless,
		chromedp.NoSandbox,
		chromedp.DisableGPU,
	)
	if *chromePath != "" {
		opts = append(opts, chromedp.ExecPath(*chromePath))
	}
	allocCtx, cancel := chromedp.NewExecAllocator(context.Background(), opts...)
	defer cancel()

	// Boot the parent browser once so per-test NewContext only
	// allocates a tab, not a Chrome process.
	parent, parentCancel := chromedp.NewContext(allocCtx)
	defer parentCancel()
	if err := chromedp.Run(parent); err != nil {
		fmt.Fprintln(os.Stderr, "chromedp launch:", err)
		os.Exit(2)
	}

	tests := buildTests(100)

	start := time.Now()

	// Worker pool. chromedp serializes commands per-context, so we
	// fan out by spawning N workers each pulling tests off a chan.
	queue := make(chan tc, len(tests))
	for _, t := range tests {
		queue <- t
	}
	close(queue)

	var wg sync.WaitGroup
	failed := 0
	var failMu sync.Mutex
	for w := 0; w < *workers; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for t := range queue {
				if err := runOne(parent, t); err != nil {
					failMu.Lock()
					failed++
					if failed <= 3 {
						fmt.Fprintf(os.Stderr, "  test %d: %v\n", t.i, err)
					}
					failMu.Unlock()
				}
			}
		}()
	}
	wg.Wait()

	elapsed := time.Since(start).Milliseconds()
	if failed > 0 {
		fmt.Fprintf(os.Stderr, "FAILED: %d of %d\n", failed, len(tests))
	}
	fmt.Println(elapsed)
}
