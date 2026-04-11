Feature: Counter app
  Interactive counter using the counter fixture.

  Scenario: Increment counter multiple times
    Given I navigate to "/counter.html"
    Then "#display" should have text "0"
    When I click "button"
    Then "#display" should have text "1"
    When I click "button"
    And I click "button"
    Then "#display" should have text "3"

  Scenario: Counter tracks clicks accurately
    Given I navigate to "/counter.html"
    When I click "button"
    And I click "button"
    And I click "button"
    And I click "button"
    And I click "button"
    Then I evaluate "window.count" and expect "5"
