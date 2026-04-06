Feature: Data Tables
  Inline data tables can be passed to steps.

  Scenario: Set multiple cookies from a table
    Given I navigate to "https://example.com"
    When I set cookie "key1" to "val1"
    And I set cookie "key2" to "val2"
    And I evaluate "document.title = document.cookie"
    Then the page title should contain "key1=val1"
    And the page title should contain "key2=val2"

  Scenario: Set and clear local storage entries
    Given I navigate to "https://example.com"
    When I set local storage "color" to "red"
    And I set local storage "size" to "large"
    And I evaluate "document.title = localStorage.getItem('color') + '-' + localStorage.getItem('size')"
    Then the page title should be "red-large"
    And I clear local storage
    And I evaluate "document.title = localStorage.length.toString()"
    Then the page title should be "0"
