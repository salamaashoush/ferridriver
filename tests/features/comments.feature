# This is a comment at the top of the file
Feature: Comments
  # This is a comment in the description area
  Gherkin supports # comments on their own lines.

  # Comment before a scenario
  Scenario: Steps work with comments around them
    # Comment before a step
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    # Another comment between steps
    Then the page title should be "Example Domain"
    # Trailing comment
    And "h1" should be visible
