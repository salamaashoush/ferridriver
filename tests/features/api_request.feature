Feature: API request fixture

  Scenario: GET request to public API
    When I send a GET request to "https://httpbin.org/get"
    Then the API response status should be 200
    And the API response should be successful
    And the API response body should contain "httpbin.org"
    And the API response header "content-type" should contain "json"

  Scenario: POST request with inline JSON
    When I send a POST request to "https://httpbin.org/post" with body:
      """
      {"name": "Alice", "role": "admin"}
      """
    Then the API response status should be 200
    And the API response body should contain "Alice"
    And the API response body should contain "admin"

  Scenario: DELETE request
    When I send a DELETE request to "https://httpbin.org/delete"
    Then the API response status should be 200
    And the API response should be successful
