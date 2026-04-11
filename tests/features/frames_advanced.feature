Feature: Frames advanced
  Complex iframe scenarios using test fixtures served locally.

  Scenario: Switch to named iframe and verify content
    Given I navigate to "/frames/one-frame.html"
    When I switch to frame "child"
    Then I evaluate "document.querySelector('h2').textContent" in the active frame and expect "Frame Content"

  Scenario: Two frames with independent content
    Given I navigate to "/frames/two-frames.html"
    Then I should see 3 frames

  Scenario: Interact with frame button
    Given I navigate to "/frames/one-frame.html"
    When I switch to frame "child"
    And I evaluate "document.getElementById('frame-button').click()" in the active frame
    Then I evaluate "document.getElementById('frame-button').textContent" in the active frame and expect "Clicked!"
