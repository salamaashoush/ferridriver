Feature: API request fixture

  The requests target the local fixture server's /_api/ echo endpoints
  (an httpbin-shaped JSON echo) so the suite never depends on an
  external service being reachable.

  Scenario: GET request to the fixture API
    When I send a GET request to "/_api/get"
    Then the API response status should be 200
    And the API response should be successful
    And the API response body should contain "_api/get"
    And the API response header "content-type" should contain "json"

  Scenario: POST request with inline JSON
    When I send a POST request to "/_api/post" with body:
      """
      {"name": "Alice", "role": "admin"}
      """
    Then the API response status should be 200
    And the API response body should contain "Alice"
    And the API response body should contain "admin"

  Scenario: DELETE request
    When I send a DELETE request to "/_api/delete"
    Then the API response status should be 200
    And the API response should be successful
