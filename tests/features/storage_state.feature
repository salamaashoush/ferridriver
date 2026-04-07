Feature: Storage state save and load

  Scenario: Save and load storage state via file
    Given I navigate to "https://example.com"
    When I set local storage "token" to "abc123"
    When I set local storage "theme" to "dark"
    And I save the storage state to "mocks/auth-state.json"
    Given I navigate to "https://example.com"
    And I clear local storage
    And I load the storage state from "mocks/auth-state.json"
    Then I evaluate "document.title = localStorage.getItem('token') || 'missing'"
    And the page title should contain "abc123"

  Scenario: Storage state preserves cookies
    Given I navigate to "https://example.com"
    When I evaluate "document.cookie = 'session=xyz789; path=/'"
    And I save the storage state to "mocks/cookie-state.json"
    Given I navigate to "https://example.com"
    And I load the storage state from "mocks/cookie-state.json"
    Then I evaluate "document.title = document.cookie.includes('session=xyz789') ? 'has-cookie' : 'no-cookie'"
    And the page title should contain "has-cookie"
