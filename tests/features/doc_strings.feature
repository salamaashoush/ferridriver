Feature: Doc Strings
  Multi-line doc strings passed to steps.

  Scenario: Execute JavaScript from doc string
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    When I evaluate "document.title = 'Doc String Test'"
    Then the page title should be "Doc String Test"

  Scenario: Set page content via JavaScript with long expression
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    When I evaluate "document.querySelector('h1').textContent = 'Modified'"
    Then "h1" should have text "Modified"
