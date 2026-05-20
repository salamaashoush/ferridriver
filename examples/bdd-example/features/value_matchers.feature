Feature: Rust value matchers and asymmetric matchers in BDD step bodies
  Demonstrates the Jest-compatible value matchers (`expect_value`) and
  asymmetric matchers (`expect.any`/`objectContaining`/...) being called
  from Rust BDD step bodies. No browser dependency — pure Rust
  assertion checks against synthetic data.

  Scenario: Deep equality on a JSON object
    Given a synthetic JSON document
    Then the document equals the expected shape

  Scenario: Asymmetric matchers with any and objectContaining
    Given a synthetic JSON document
    Then the document matches an asymmetric expected shape

  Scenario: toThrow captures a synchronous Rust panic-equivalent
    Then a closure that throws is caught by toThrow
