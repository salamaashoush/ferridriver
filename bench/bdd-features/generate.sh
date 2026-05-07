#!/usr/bin/env bash
# Generate BDD feature files for the bench head-to-head.
#
# Generates Gherkin Scenario Outlines for both ferridriver bdd and
# playwright-bdd. ~1000 scenarios total (250 todos + 100 forms-valid
# + 100 forms-invalid + 50 blog-list + 200 blog-detail +
# 200 dashboard + 100 wizard).
#
# Output: bench/bdd-features/generated/*.feature
set -euo pipefail

cd "$(dirname "$0")"
OUT="generated"
rm -rf "$OUT" && mkdir -p "$OUT"

# ── todos: 250 add-and-verify scenarios ───────────────────────────────
{
  echo 'Feature: Todo list'
  echo
  echo '  Scenario Outline: todo add and verify <i>'
  echo '    Given I navigate to "/todos"'
  echo '    When I fill "[data-testid=todo-input]" with "learn ferridriver <i>"'
  echo '    When I click "[data-testid=todo-add]"'
  echo '    Then "[data-testid=todo-list] li" should contain text "learn ferridriver <i>"'
  echo '    Then "[data-testid=remaining-count]" should contain text "1 left"'
  echo
  echo '    Examples:'
  echo '      | i |'
  for i in $(seq 0 249); do printf '      | %d |\n' "$i"; done
} > "$OUT/todos.feature"

# ── forms: 100 valid submits + 100 invalid-field probes ───────────────
{
  echo 'Feature: Forms — valid submissions'
  echo
  echo '  Scenario Outline: form submit valid <i>'
  echo '    Given I navigate to "/forms"'
  echo '    When I fill "[data-testid=form-name]" with "Tester <i>"'
  echo '    When I fill "[data-testid=form-email]" with "tester<i>@example.com"'
  echo '    When I fill "[data-testid=form-age]" with "<age>"'
  echo '    When I select "<role>" from "[data-testid=form-role]"'
  echo '    When I fill "[data-testid=form-bio]" with "bio for tester <i>"'
  echo '    When I check "[data-testid=form-agree]"'
  echo '    When I click "[data-testid=form-submit]"'
  echo '    Then "[data-testid=submit-result]" should be visible'
  echo '    Then "[data-testid=submit-payload]" should contain text "Tester <i>"'
  echo
  echo '    Examples:'
  echo '      | i | age | role  |'
  ROLES=(user admin guest)
  for i in $(seq 0 99); do
    age=$((20 + i % 50))
    role=${ROLES[$((i % 3))]}
    printf '      | %d | %d | %s |\n' "$i" "$age" "$role"
  done
} > "$OUT/forms-valid.feature"

# ── forms invalid: rotate over 4 invalid-field variants ──────────────
{
  echo 'Feature: Forms — invalid field validation'
  echo
  echo '  Scenario Outline: form invalid <i> field <field>'
  echo '    Given I navigate to "/forms"'
  echo '    When I fill "[data-testid=<field>]" with "<value>"'
  echo '    When I click "[data-testid=form-submit]"'
  echo '    Then "[data-testid=<error>]" should be visible'
  echo '    Then "[data-testid=<error>]" should contain text "<message>"'
  echo
  echo '    Examples:'
  echo '      | i | field      | value     | error      | message      |'
  declare -a fields=(form-name form-email form-age form-bio)
  declare -a values=("x" "not-an-email" "5" "long-bio")
  declare -a errors=(error-name error-email error-age error-bio)
  declare -a messages=("at least 2" "invalid" "13 or older" "280")
  for i in $(seq 0 99); do
    j=$((i % 4))
    val="${values[$j]}"
    if [ "$j" = 3 ]; then val=$(printf 'a%.0s' $(seq 1 300)); fi
    printf '      | %d | %s | %s | %s | %s |\n' "$i" "${fields[$j]}" "$val" "${errors[$j]}" "${messages[$j]}"
  done
} > "$OUT/forms-invalid.feature"

# ── blog list: 50 search-by-tag scenarios ────────────────────────────
{
  echo 'Feature: Blog — list and search'
  echo
  echo '  Scenario Outline: blog search by tag <tag> #<i>'
  echo '    Given I navigate to "/blog"'
  echo '    Then "[data-testid=blog-list] li" should be visible'
  echo '    When I fill "[data-testid=blog-search]" with "<tag>"'
  echo '    Then "[data-testid=blog-count]" should contain text "matches"'
  echo '    Then "[data-testid=blog-list] li" should be visible'
  echo
  echo '    Examples:'
  echo '      | i | tag        |'
  TAGS=(rust typescript react cdp perf web ai api)
  for i in $(seq 0 49); do
    printf '      | %d | %s |\n' "$i" "${TAGS[$((i % 8))]}"
  done
} > "$OUT/blog-list.feature"

# ── blog detail: 200 post-detail walks ───────────────────────────────
{
  echo 'Feature: Blog — post detail'
  echo
  echo '  Scenario Outline: blog detail <slug>'
  echo '    Given I navigate to "/blog/<slug>"'
  echo '    Then "[data-testid=post-title]" should contain text "Post <i>:"'
  echo '    Then "[data-testid=post-body]" should contain text "lorem ipsum"'
  echo '    When I click "[data-testid=back-link]"'
  echo '    Then "[data-testid=blog-title]" should be visible'
  echo
  echo '    Examples:'
  echo '      | i | slug         |'
  for i in $(seq 0 199); do
    slug=$(printf 'post-%03d' "$i")
    printf '      | %d | %s |\n' "$i" "$slug"
  done
} > "$OUT/blog-detail.feature"

# ── dashboard: 200 filter-combo scenarios ────────────────────────────
{
  echo 'Feature: Dashboard — filters and sort'
  echo
  echo '  Scenario Outline: dashboard <region>/<status>/<sort> #<i>'
  echo '    Given I navigate to "/dashboard"'
  echo '    Then "[data-testid=sales-table]" should be visible'
  echo '    When I select "<region>" from "[data-testid=region-filter]"'
  echo '    When I select "<status>" from "[data-testid=status-filter]"'
  echo '    When I select "<sort>" from "[data-testid=sort-by]"'
  echo '    Then "[data-testid=row-count]" should contain text "rows"'
  echo '    Then "[data-testid=total-amount]" should contain text "$"'
  echo
  echo '    Examples:'
  echo '      | i | region | status    | sort   |'
  REGIONS=(all NA EU APAC)
  STATUSES=(all pending shipped delivered returned)
  SORTS=(amount date)
  i=0
  for region in "${REGIONS[@]}"; do
    for status in "${STATUSES[@]}"; do
      for sort in "${SORTS[@]}"; do
        for _ in $(seq 1 5); do
          printf '      | %d | %s | %s | %s |\n' "$i" "$region" "$status" "$sort"
          i=$((i + 1))
        done
      done
    done
  done
} > "$OUT/dashboard.feature"

# ── wizard: 100 multi-step flows ─────────────────────────────────────
{
  echo 'Feature: Wizard — multi-step flow'
  echo
  echo '  Scenario Outline: wizard end-to-end #<i>'
  echo '    Given I navigate to "/wizard"'
  echo '    When I fill "[data-testid=wiz-username]" with "user<i>"'
  echo '    When I fill "[data-testid=wiz-password]" with "secret<i>"'
  echo '    When I click "[data-testid=wiz-next]"'
  echo '    When I fill "[data-testid=wiz-display]" with "Display <i>"'
  echo '    When I fill "[data-testid=wiz-tagline]" with "Tagline <i>"'
  echo '    When I click "[data-testid=wiz-next]"'
  echo '    When I click "[data-testid=wiz-next]"'
  echo '    Then "[data-testid=review-username]" should contain text "user<i>"'
  echo
  echo '    Examples:'
  echo '      | i |'
  for i in $(seq 0 99); do printf '      | %d |\n' "$i"; done
} > "$OUT/wizard.feature"

scenario_total=$(grep -h '^      | ' "$OUT"/*.feature | wc -l)
# subtract one header row per file
files=$(ls "$OUT" | wc -l)
echo "Generated $files feature files, $((scenario_total - files)) total scenarios in $OUT/"
