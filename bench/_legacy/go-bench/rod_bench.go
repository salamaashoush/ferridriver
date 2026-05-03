// go-rod head-to-head bench. Same shape as chromedp_bench.go.
//
// Build:  go build -tags rod -o rod-bench rod_bench.go
// Args:   --workers N --chrome <path>
// Output: total wall time ms on first stdout line.
//
//go:build rod

package main

import (
	"flag"
	"fmt"
	"net/url"
	"os"
	"sync"
	"time"

	"github.com/go-rod/rod"
	"github.com/go-rod/rod/lib/launcher"
	"github.com/go-rod/rod/lib/proto"
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

func runOne(browser *rod.Browser, t tc) error {
	page, err := browser.Page(proto.TargetCreateTarget{})
	if err != nil {
		return err
	}
	defer page.Close()

	navigate := func(url string) error {
		wait := page.WaitNavigation(proto.PageLifecycleEventNameLoad)
		if err := page.Navigate(url); err != nil {
			return err
		}
		wait()
		return nil
	}

	switch t.kind {
	case kNav:
		want := fmt.Sprintf("Test %d", t.i)
		if err := navigate(dataURL(fmt.Sprintf(
			"<title>%s</title><body><h1>Page %d</h1></body>", want, t.i))); err != nil {
			return err
		}
		info, err := page.Info()
		if err != nil {
			return err
		}
		if info.Title != want {
			return fmt.Errorf("nav: got %q want %q", info.Title, want)
		}
	case kClick:
		want := fmt.Sprintf("done %d", t.i)
		if err := navigate(dataURL(fmt.Sprintf(
			"<button id='btn' onclick=\"this.textContent='%s'\">Click %d</button>", want, t.i))); err != nil {
			return err
		}
		btn, err := page.Element("#btn")
		if err != nil {
			return err
		}
		if err := btn.Click("left", 1); err != nil {
			return err
		}
		deadline := time.Now().Add(5 * time.Second)
		for time.Now().Before(deadline) {
			tx, err := btn.Text()
			if err == nil && tx == want {
				return nil
			}
			time.Sleep(20 * time.Millisecond)
		}
		return fmt.Errorf("click: text never reached %q", want)
	case kEval:
		want := fmt.Sprintf("%d", t.i)
		if err := navigate(dataURL(fmt.Sprintf(
			"<title>Eval %d</title><div id='out'>%d</div>", t.i, t.i))); err != nil {
			return err
		}
		v, err := page.Eval("() => document.getElementById('out')?.textContent")
		if err != nil {
			return err
		}
		got := v.Value.Str()
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

	l := launcher.New().Headless(true).NoSandbox(true)
	if *chromePath != "" {
		l = l.Bin(*chromePath)
	}
	wsURL, err := l.Launch()
	if err != nil {
		fmt.Fprintln(os.Stderr, "rod launch:", err)
		os.Exit(2)
	}
	defer l.Cleanup()
	browser := rod.New().ControlURL(wsURL).MustConnect()
	defer browser.Close()

	tests := buildTests(100)
	queue := make(chan tc, len(tests))
	for _, t := range tests {
		queue <- t
	}
	close(queue)

	start := time.Now()
	var wg sync.WaitGroup
	failed := 0
	var failMu sync.Mutex
	for w := 0; w < *workers; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for t := range queue {
				if err := runOne(browser, t); err != nil {
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
