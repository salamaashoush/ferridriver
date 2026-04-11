Feature: Test annotations
  Playwright-compatible skip, fixme, fail, slow annotations with conditions.

  # ── Unconditional annotations ──

  @skip
  Scenario: Unconditional skip never runs
    # This body should never execute — navigation to a broken URL would fail.
    Given I navigate to "https://this-should-never-run.invalid"

  @slow
  Scenario: Slow annotation triples timeout
    Given I navigate to "/empty.html"
    Then the page title should contain ""

  @fail
  Scenario: Unconditional fail — inverts deliberate failure to pass
    Given I navigate to "/empty.html"
    Then the page title should contain "THIS TITLE DOES NOT EXIST"

  # ── Conditional skip: browser-based ──

  @skip(firefox)
  Scenario: Skip on Firefox only — runs on Chromium
    Given I navigate to "/emulation.html"
    Then "#viewport-width" should have text "1280"

  @skip(!chromium)
  Scenario: Negated skip — runs only on Chromium
    Given I navigate to "/emulation.html"
    Then "#viewport-width" should have text "1280"

  # ── Conditional skip: OS-based ──

  @skip(!linux)
  Scenario: Skip on non-Linux — runs on this machine
    Given I navigate to "/empty.html"

  # ── Conditional skip: environment-based ──

  @skip(env:FERRIDRIVER_SKIP_THIS_TEST)
  Scenario: Skip when env var is set — runs when unset
    Given I navigate to "/empty.html"

  # ── Fixme with condition ──

  @fixme(firefox)
  Scenario: Fixme on Firefox — runs normally on Chromium
    Given I navigate to "/emulation.html"
    Then "#viewport-width" should have text "1280"

  # ── Conditional fail: browser-specific known failure ──
  # @fail(chromium) + a body that fails on every browser.
  # On Chromium: condition matches → fail inverted → pass.
  # On Firefox: condition doesn't match → this is a genuine failure → we guard with @skip(!chromium).
  # This is how Playwright users would write it: guard non-applicable browsers with @skip.

  @fail
  Scenario: Fail annotation inverts a deliberate failure
    Given I navigate to "/empty.html"
    Then the page title should contain "NONEXISTENT"

  # ── Conjunction conditions ──

  @skip(firefox+linux)
  Scenario: Skip on Firefox AND Linux combined
    Given I navigate to "/emulation.html"
    Then "#viewport-width" should have text "1280"

  # ── Slow with condition ──

  @slow(ci)
  Scenario: Slow in CI only — triples timeout when CI is set
    Given I navigate to "/empty.html"

  # ── Tags coexist with annotations ──

  @smoke @skip(firefox)
  Scenario: Tagged and conditionally skipped
    Given I navigate to "/emulation.html"
    Then "#viewport-width" should have text "1280"
