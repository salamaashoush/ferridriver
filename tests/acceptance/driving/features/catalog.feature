Feature: Catalog

  @smoke
  Scenario: the catalog lists what the API served
    Given the catalog page is open
    When the catalog loads
    Then it shows 2 items

  @slow
  Scenario: a second pass reuses the router
    Given the catalog page is open
    When the catalog loads
    Then the router served "/catalog"
