Feature: Network advanced
  Advanced network interception with the network test fixture.

  Scenario: Mock API response with custom headers
    Given I navigate to "/network.html"
    And I mock requests to "**/api/data" with status 200 and body '{"items":[1,2,3]}'
    When I evaluate "doFetch('/api/data')"
    Then I evaluate "window.lastStatus" and expect "200"
    And I evaluate "window.lastResponse" and expect '{"items":[1,2,3]}'

  Scenario: Mock POST endpoint
    Given I navigate to "/network.html"
    And I mock requests to "**/api/submit" with status 201 and body '{"id":42,"created":true}'
    When I evaluate "doPost('/api/submit', {name:'test'})"
    Then I evaluate "window.lastStatus" and expect "201"

  Scenario: Block resource and verify error
    Given I navigate to "/network.html"
    And I block requests to "**/api/blocked"
    When I evaluate "fetch('/api/blocked').then(()=>window.blocked=false).catch(()=>window.blocked=true)"
    Then I evaluate "window.blocked" and expect "true"

  Scenario: Intercept multiple routes
    Given I navigate to "/network.html"
    And I mock requests to "**/api/a" with JSON '{"route":"a"}'
    And I mock requests to "**/api/b" with JSON '{"route":"b"}'
    When I evaluate "doFetch('/api/a')"
    Then I evaluate "window.lastResponse" and expect '{"route":"a"}'
    When I evaluate "doFetch('/api/b')"
    Then I evaluate "window.lastResponse" and expect '{"route":"b"}'
