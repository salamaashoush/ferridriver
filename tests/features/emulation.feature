Feature: Emulation
  Viewport, timezone, locale, and media emulation.

  Scenario: Set timezone and verify
    Given I navigate to "/emulation.html"
    And I set timezone to "America/New_York"
    And I reload the page
    Then "#timezone" should have text "America/New_York"

  Scenario: Set locale and verify
    Given I set locale to "de-DE"
    And I navigate to "/emulation.html"
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
