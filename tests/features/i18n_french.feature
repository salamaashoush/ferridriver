# language: fr
Fonctionnalité: Support i18n
  Les mots-clés Gherkin en français sont reconnus.

  Scénario: Navigation en français
    Soit I navigate to "https://example.com"
    Alors the page title should contain "Example"
    Et "h1" should be visible

  Scénario: Assertions en français
    Soit I navigate to "https://example.com"
    Alors "h1" should have text "Example Domain"
    Et the URL should contain "example.com"
