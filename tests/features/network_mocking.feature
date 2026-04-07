Feature: Network mocking

  Scenario: Mock page navigation with inline body
    Given I mock requests to "**/mock-page" with status 200 and body "<html><head><title>Inline Mock</title></head><body>Hello</body></html>"
    When I navigate to "http://mock.test/mock-page"
    Then the page title should contain "Inline Mock"

  Scenario: Mock page with fixture file
    Given I mock requests to "**/fixture-page" with fixture "mocks/page.html"
    When I navigate to "http://mock.test/fixture-page"
    Then the page title should contain "Mocked Page"

  Scenario: Mock API with inline JSON
    Given I navigate to "https://example.com"
    And I mock requests to "**/api/inline" with JSON '{"ok":true}'
    When I fetch "/api/inline"
    Then the response status should be 200
    And the response body should contain "ok"

  Scenario: Mock API with JSON fixture file
    Given I navigate to "https://example.com"
    And I mock requests to "**/api/users" with fixture "mocks/users.json"
    When I fetch "/api/users"
    Then the response status should be 200
    And the response body should contain "Alice"
    And the response body should contain "Bob"

  Scenario: Mock with custom status
    Given I navigate to "https://example.com"
    And I mock requests to "**/api/created" with status 201 and body '{"id":42}'
    When I fetch "/api/created"
    Then the response status should be 201
    And the response body should contain "42"

  Scenario: Block requests
    Given I navigate to "https://example.com"
    And I block requests to "**/blocked-resource"
    When I evaluate "fetch('/blocked-resource').then(()=>document.title='ok').catch(()=>document.title='blocked')"
    Then the page title should contain "blocked"

  Scenario: Intercept and assert requests
    Given I navigate to "https://example.com"
    And I intercept requests to "**/api/tracked"
    When I fetch "/api/tracked"
    Then a request to "/api/tracked" should have been made

  Scenario: Unroute removes interception
    Given I navigate to "https://example.com"
    And I mock requests to "**/api/temp" with JSON '{"temp":true}'
    When I fetch "/api/temp"
    Then the response body should contain "temp"
    When I remove route for "**/api/temp"
