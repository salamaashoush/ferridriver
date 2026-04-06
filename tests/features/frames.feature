@frame
Feature: Frames
  Iframe switching and interaction.

  Scenario: Switch to iframe by name and interact
    Given I navigate to "data:text/html,<iframe name=\"child\" srcdoc=\"<h1>Inside Frame</h1>\"></iframe>"
    When I switch to frame "child"
    Then the frame "child" should exist

  Scenario: Switch back to main frame
    Given I navigate to "data:text/html,<h1>Main</h1><iframe name=\"child\" srcdoc=\"<p>Frame Content</p>\"></iframe>"
    When I switch to frame "child"
    And I switch to main frame
    Then "h1" should contain text "Main"

  Scenario: Count frames on a page
    Given I navigate to "data:text/html,<iframe name=\"a\" srcdoc=\"<p>A</p>\"></iframe><iframe name=\"b\" srcdoc=\"<p>B</p>\"></iframe>"
    Then I should see 3 frames
