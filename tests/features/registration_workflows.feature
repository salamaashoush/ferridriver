Feature: Registration workflows
  Registration should preserve entered details and require valid details and consent.

  Background:
    Given Salama has filled the required registration details

  Scenario: Submit a registration without opting into the newsletter
    When I submit the registration
    Then the registration receipt contains:
      | fullname | Salama Ashoush      |
      | email    | salama@example.com |
      | country  | de                 |
      | terms    | on                 |
      | contact  | email              |
    And the registration does not subscribe to the newsletter

  Scenario: Submit optional preferences with the registration
    When I check "#newsletter"
    And I check "input[name='contact'][value='phone']"
    And I fill "#phone" with "+1 555 0100"
    And I fill "#bio" with "Building browser tools."
    And I submit the registration
    Then the registration receipt contains:
      | fullname   | Salama Ashoush          |
      | email      | salama@example.com     |
      | newsletter | on                     |
      | contact    | phone                  |
      | phone      | +1 555 0100            |
      | bio        | Building browser tools. |

  Scenario: Withdraw consent and recover without losing entered details
    When I uncheck "#terms"
    And I submit the registration
    Then the registration is blocked by "#terms"
    And "#fullname" should have value "Salama Ashoush"
    And "#email" should have value "salama@example.com"
    When I check "#terms"
    And I submit the registration
    Then the registration receipt contains:
      | fullname | Salama Ashoush      |
      | email    | salama@example.com |
      | terms    | on                 |

  Scenario Outline: Correct invalid details and submit the same form
    When I fill "<field>" with "<invalid>"
    And I submit the registration
    Then the registration is blocked by "<field>"
    And "#country" should have value "de"
    When I fill "<field>" with "<valid>"
    And I submit the registration
    Then the registration receipt contains:
      | fullname | Salama Ashoush      |
      | email    | salama@example.com |

    Examples:
      | field     | invalid    | valid               |
      | #email    | not-email  | salama@example.com  |
      | #fullname |            | Salama Ashoush      |

  Scenario: Reset clears the draft and restores default preferences
    When I check "#newsletter"
    And I check "input[name='contact'][value='phone']"
    And I click "#reset-btn"
    Then "#fullname" should have value ""
    And "#email" should have value ""
    And "#password" should have value ""
    And "#confirm-password" should have value ""
    And "#country" should have value ""
    And "#terms" should not be checked
    And "#newsletter" should not be checked
    And "input[name='contact'][value='email']" should be checked
    When I submit the registration
    Then the registration is blocked by "#fullname"
