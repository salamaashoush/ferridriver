@frame
Feature: Frames
  Iframe switching and interaction.

  Scenario: Switch to iframe by name and interact
    Given I navigate to "https://example.com"
    And I evaluate "document.body.innerHTML = '<iframe name=child src=about:blank></iframe>'"
    When I switch to frame "child"
    Then the frame "child" should exist

  Scenario: Switch back to main frame
    Given I navigate to "https://example.com"
    And I evaluate "document.body.innerHTML = '<h1>Main</h1><iframe name=child src=about:blank></iframe>'"
    When I switch to frame "child"
    And I switch to main frame
    Then "h1" should contain text "Main"

  Scenario: Count frames on a page
    Given I navigate to "https://example.com"
    And I evaluate "document.body.innerHTML = '<iframe name=a src=about:blank></iframe><iframe name=b src=about:blank></iframe>'"
    Then I should see 3 frames
