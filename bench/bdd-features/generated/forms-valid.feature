Feature: Forms — valid submissions

  Scenario Outline: form submit valid <i>
    Given I navigate to "/forms"
    When I fill "[data-testid=form-name]" with "Tester <i>"
    When I fill "[data-testid=form-email]" with "tester<i>@example.com"
    When I fill "[data-testid=form-age]" with "<age>"
    When I select "<role>" from "[data-testid=form-role]"
    When I fill "[data-testid=form-bio]" with "bio for tester <i>"
    When I check "[data-testid=form-agree]"
    When I click "[data-testid=form-submit]"
    Then "[data-testid=submit-result]" should be visible
    Then "[data-testid=submit-payload]" should contain text "Tester <i>"

    Examples:
      | i | age | role  |
      | 0 | 20 | user |
      | 1 | 21 | admin |
      | 2 | 22 | guest |
      | 3 | 23 | user |
      | 4 | 24 | admin |
      | 5 | 25 | guest |
      | 6 | 26 | user |
      | 7 | 27 | admin |
      | 8 | 28 | guest |
      | 9 | 29 | user |
      | 10 | 30 | admin |
      | 11 | 31 | guest |
      | 12 | 32 | user |
      | 13 | 33 | admin |
      | 14 | 34 | guest |
      | 15 | 35 | user |
      | 16 | 36 | admin |
      | 17 | 37 | guest |
      | 18 | 38 | user |
      | 19 | 39 | admin |
      | 20 | 40 | guest |
      | 21 | 41 | user |
      | 22 | 42 | admin |
      | 23 | 43 | guest |
      | 24 | 44 | user |
      | 25 | 45 | admin |
      | 26 | 46 | guest |
      | 27 | 47 | user |
      | 28 | 48 | admin |
      | 29 | 49 | guest |
      | 30 | 50 | user |
      | 31 | 51 | admin |
      | 32 | 52 | guest |
      | 33 | 53 | user |
      | 34 | 54 | admin |
      | 35 | 55 | guest |
      | 36 | 56 | user |
      | 37 | 57 | admin |
      | 38 | 58 | guest |
      | 39 | 59 | user |
      | 40 | 60 | admin |
      | 41 | 61 | guest |
      | 42 | 62 | user |
      | 43 | 63 | admin |
      | 44 | 64 | guest |
      | 45 | 65 | user |
      | 46 | 66 | admin |
      | 47 | 67 | guest |
      | 48 | 68 | user |
      | 49 | 69 | admin |
      | 50 | 20 | guest |
      | 51 | 21 | user |
      | 52 | 22 | admin |
      | 53 | 23 | guest |
      | 54 | 24 | user |
      | 55 | 25 | admin |
      | 56 | 26 | guest |
      | 57 | 27 | user |
      | 58 | 28 | admin |
      | 59 | 29 | guest |
      | 60 | 30 | user |
      | 61 | 31 | admin |
      | 62 | 32 | guest |
      | 63 | 33 | user |
      | 64 | 34 | admin |
      | 65 | 35 | guest |
      | 66 | 36 | user |
      | 67 | 37 | admin |
      | 68 | 38 | guest |
      | 69 | 39 | user |
      | 70 | 40 | admin |
      | 71 | 41 | guest |
      | 72 | 42 | user |
      | 73 | 43 | admin |
      | 74 | 44 | guest |
      | 75 | 45 | user |
      | 76 | 46 | admin |
      | 77 | 47 | guest |
      | 78 | 48 | user |
      | 79 | 49 | admin |
      | 80 | 50 | guest |
      | 81 | 51 | user |
      | 82 | 52 | admin |
      | 83 | 53 | guest |
      | 84 | 54 | user |
      | 85 | 55 | admin |
      | 86 | 56 | guest |
      | 87 | 57 | user |
      | 88 | 58 | admin |
      | 89 | 59 | guest |
      | 90 | 60 | user |
      | 91 | 61 | admin |
      | 92 | 62 | guest |
      | 93 | 63 | user |
      | 94 | 64 | admin |
      | 95 | 65 | guest |
      | 96 | 66 | user |
      | 97 | 67 | admin |
      | 98 | 68 | guest |
      | 99 | 69 | user |
