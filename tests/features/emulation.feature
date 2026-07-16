Feature: Emulation
  Viewport, timezone, locale, and media emulation.

  Scenario: Set timezone and verify
    Given I navigate to "/emulation.html"
    And I set timezone to "America/New_York"
    And I reload the page
    Then "#timezone" should have text "America/New_York"

  # locale is a context-creation option (`test.use` analog): WebKit web
  # processes latch languages at spawn, so it must be present before the
  # page's process exists — the @use tag feeds the worker's context setup.
  @use(locale=de-DE)
  Scenario: Set locale and verify
    When I navigate to "/emulation.html"
    Then "#language" should contain text "de"

  @skip(firefox)
  Scenario: Emulate dark color scheme
    # Requires emulation.setForcedColorsModeThemeOverride (not yet in Firefox/BiDi)
    Given I emulate color scheme "dark"
    And I navigate to "/emulation.html"
    Then "#color-scheme" should have text "dark"

  Scenario: Viewport dimensions are correct
    Given I navigate to "/emulation.html"
    Then "#viewport-width" should have text "1280"
    And "#viewport-height" should have text "720"

  Scenario: Set viewport size
    Given I set viewport to 800x600
    And I navigate to "/emulation.html"
    Then "#viewport-width" should have text "800"
    And "#viewport-height" should have text "600"
