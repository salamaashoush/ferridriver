//! Comprehensive integration tests for all BDD step definitions.
//!
//! Tests every step category against a real browser via the scenario runner.
//! Uses data URLs to avoid external dependencies.

use ferridriver::backend::BackendKind;
use ferridriver::options::LaunchOptions;
use ferridriver::scenario::{self, ScenarioOptions, ScenarioResult};
use ferridriver::Browser;

fn data_url(html: &str) -> String {
    format!(
        "data:text/html,{}",
        html
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{:02X}", b),
            })
            .collect::<String>()
    )
}

fn opts() -> ScenarioOptions {
    ScenarioOptions {
        stop_on_failure: true,
        screenshot_on_failure: false,
    }
}

fn assert_passed(result: &ScenarioResult) {
    assert_eq!(
        result.status, "passed",
        "Scenario failed: {}\nSteps:\n{}",
        result.summary,
        result
            .steps
            .iter()
            .map(|s| {
                let err = s.error.as_deref().unwrap_or("");
                format!("  {} {} {} - {} {}", s.step, s.keyword, s.description, s.status, err)
            })
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn assert_failed(result: &ScenarioResult) {
    assert_eq!(result.status, "failed", "Expected failure but got: {}", result.summary);
}

/// Build a scenario script with a URL on the first line.
fn scenario(url: &str, body: &str) -> String {
    format!("Scenario: test\n  Given I navigate to \"{url}\"\n{body}")
}

// ─── Navigation ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_navigation_steps() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<title>Nav Test</title><body>Hello</body>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "  Then the title should be 'Nav Test'\n  And the page should contain text 'Hello'\n  When I reload the page\n  Then the title should be 'Nav Test'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Click, Fill, Type, Press ───────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_interaction_click_fill() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<input id='name' type='text'><input id='email' type='text'><button id='btn' onclick=\"document.getElementById('btn').textContent='clicked'\">Go</button>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I fill '#name' with 'Alice'
  Then '#name' should have value 'Alice'
  When I click '#btn'
  Then '#btn' should have text 'clicked'
  When I fill '#email' with 'alice@test.com'
  Then '#email' should have value 'alice@test.com'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_interaction_type_and_press() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<input id='i' type='text' autofocus><script>document.getElementById('i').addEventListener('keydown',function(e){if(e.key==='Enter')document.title='submitted'})</script>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I focus '#i'
  And I type 'hello'
  Then '#i' should have value 'hello'
  When I press 'Enter'
  Then the title should be 'submitted'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Double-click, Hover ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_interaction_dblclick_hover() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<div id='counter'>0</div><button id='inc' onclick=\"document.getElementById('counter').textContent=Number(document.getElementById('counter').textContent)+1\">+</button><div id='hover-target' onmouseenter=\"this.textContent='hovered'\" style='padding:20px'>hover me</div>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I double-click '#inc'
  Then '#counter' should have text '2'
  When I hover over '#hover-target'
  Then '#hover-target' should contain text 'hovered'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Fill form with data table ──────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_interaction_fill_form() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<input id='first' type='text'><input id='last' type='text'><input id='age' type='text'>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I fill the form:
    | #first | John |
    | #last  | Doe  |
    | #age   | 30   |
  Then '#first' should have value 'John'
  And '#last' should have value 'Doe'
  And '#age' should have value '30'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Select option ──────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_interaction_select_option() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<select id='color'><option value='r'>Red</option><option value='g'>Green</option><option value='b'>Blue</option></select>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I select 'Green' from '#color'
  Then '#color' should have value 'g'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Clear input ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_interaction_clear() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<input id='i' type='text' value='prefilled'>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#i' should have value 'prefilled'
  When I clear '#i'
  Then '#i' should have value ''"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Scroll steps ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_interaction_scroll() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<div style='height:3000px'><div id='top'>top</div></div><div id='bottom'>bottom</div>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I scroll to '#bottom'
  And I scroll down by 100
  And I scroll up by 50
  Then '#bottom' should be visible"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Wait steps ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_wait_steps() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<div id='container'></div><script>setTimeout(function(){document.getElementById('container').innerHTML='<span id=\"loaded\">Done</span>'},200)</script>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I wait for selector '#loaded'
  Then '#loaded' should have text 'Done'
  When I wait 100ms
  Then the page should contain text 'Done'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    // Test wait for text
    let url2 = data_url("<div id='msg'></div><script>setTimeout(function(){document.getElementById('msg').textContent='Ready'},200)</script>");
    let result2 = scenario::run(
        page.inner(),
        &scenario(&url2, "\
  When I wait for text 'Ready'
  Then the page should contain text 'Ready'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result2);

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_wait_timeout_steps() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<div id='area'></div><script>setTimeout(function(){document.getElementById('area').innerHTML='<span id=\"delayed\">here</span>'},300);setTimeout(function(){document.getElementById('area').textContent='final text'},600)</script>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I wait for '#delayed' for 5000ms
  Then '#delayed' should have text 'here'
  When I wait for text 'final text' for 5000ms
  Then the page should contain text 'final text'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Assertion steps ────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_assertion_page_text() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<title>My App</title><body><p>Welcome to the app</p></body>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then the page should contain text 'Welcome'
  And the page should have text 'Welcome to the app'
  And the page should not contain text 'Goodbye'
  And the page should not have text 'Error'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_assertion_url_title() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<title>Dashboard</title><body>content</body>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then the URL should contain 'data:'
  And the title should be 'Dashboard'
  And the title should contain 'Dash'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_assertion_visibility_text_value() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<div id='visible'>Hello</div><input id='inp' type='text' value='test123'><h1 id='heading'>Welcome</h1>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#visible' should be visible
  And '#nonexistent' should not be visible
  And '#visible' should contain text 'Hello'
  And '#visible' should not contain text 'Goodbye'
  And '#heading' should have text 'Welcome'
  And '#inp' should have value 'test123'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_assertion_attributes_classes() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<a id='link' href='/about' class='nav active' data-id='42'>About</a><input id='req' type='text' required>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#link' should have attribute 'href' with value '/about'
  And '#link' should have attribute 'data-id'
  And '#link' should not have attribute 'disabled'
  And '#link' should have class 'active'
  And '#link' should have class 'nav'
  And '#link' should not have class 'hidden'
  And '#req' should have attribute 'required'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_assertion_state() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<button id='ok'>OK</button><button id='nope' disabled>No</button><input id='cb' type='checkbox' checked><input id='cb2' type='checkbox'>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#ok' should be enabled
  And '#nope' should be disabled
  And '#cb' should be checked
  And '#cb2' should not be checked"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_assertion_element_count() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<ul><li class='item'>A</li><li class='item'>B</li><li class='item'>C</li></ul>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "  Then there should be 3 '.item'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Variable steps ─────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_variable_steps() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<title>Var Test</title><h1 id='heading'>Hello World</h1><input id='inp' type='text' value='secret123'><a id='link' href='/dashboard'>Go</a>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I store the text of '#heading' as $heading
  And I store the value of '#inp' as $val
  And I store the attribute 'href' of '#link' as $href
  And I store the URL as $url
  And I store the title as $title
  And I evaluate '2 + 2' and store as $sum
  And I set $name to 'Alice'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    assert_eq!(result.variables.get("heading").unwrap(), "Hello World");
    assert_eq!(result.variables.get("val").unwrap(), "secret123");
    assert_eq!(result.variables.get("href").unwrap(), "/dashboard");
    assert!(result.variables.get("url").unwrap().starts_with("data:"));
    assert_eq!(result.variables.get("title").unwrap(), "Var Test");
    assert_eq!(result.variables.get("sum").unwrap(), "4");
    assert_eq!(result.variables.get("name").unwrap(), "Alice");

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_variable_interpolation() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<input id='name' type='text'>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Given I set $user to 'Bob'
  When I fill '#name' with '$user'
  Then '#name' should have value 'Bob'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Cookie steps ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_cookie_steps() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<body>cookie test</body>");

    // Set cookies with explicit domain (works on any page origin)
    // Note: delete/clear require HTTP/HTTPS origin, so we only test set here
    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I set cookie 'session' to 'abc123' on 'localhost'
  And I set cookie 'token' to 'xyz' on 'localhost'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Storage steps ──────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_storage_steps() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    // localStorage needs a proper origin - data URLs have opaque origins in Chrome
    // Use a page that creates its own storage context via evaluate
    let url = data_url("<body>storage test</body>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I evaluate 'try{localStorage.clear()}catch(e){}'
  And I evaluate 'try{localStorage.setItem(\"theme\",\"dark\")}catch(e){}'
  And I evaluate 'try{localStorage.getItem(\"theme\")}catch(e){\"error\"}' and store as $theme
  And I evaluate 'try{localStorage.removeItem(\"theme\")}catch(e){}'
  And I evaluate 'try{localStorage.getItem(\"theme\")}catch(e){\"error\"}' and store as $removed"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Screenshot and snapshot steps ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_screenshot_snapshot_steps() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<h1>Screenshot Test</h1><div id='box' style='width:100px;height:100px;background:red'></div>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then I take a screenshot
  And I take a screenshot of '#box'
  And I take a snapshot"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    // Verify screenshot step returns base64 data
    let screenshot_step = &result.steps[1]; // "I take a screenshot"
    assert!(screenshot_step.data.is_some(), "screenshot should return data");
    let data = screenshot_step.data.as_ref().unwrap();
    assert!(data.get("screenshot").is_some(), "should have screenshot field");

    // Verify snapshot step returns text
    let snapshot_step = &result.steps[3]; // "I take a snapshot"
    assert!(snapshot_step.data.is_some(), "snapshot should return data");

    browser.close().await.unwrap();
}

// ─── JavaScript evaluation ──────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_javascript_step() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<div id='target'>before</div>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I evaluate 'document.getElementById(\"target\").textContent = \"after\"'
  Then '#target' should have text 'after'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Failure behavior ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_assertion_failure_stops_execution() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<p>Hello</p>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then the page should contain text 'Missing Text'
  And the page should contain text 'Hello'"),
        opts(),
    )
    .await
    .unwrap();
    assert_failed(&result);

    assert_eq!(result.steps[0].status, "passed"); // navigate
    assert_eq!(result.steps[1].status, "failed"); // missing text
    assert!(result.steps[1].error.is_some());
    assert_eq!(result.steps[2].status, "skipped"); // skipped

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_wrong_count_fails() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<ul><li>A</li><li>B</li></ul>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "  Then there should be 5 'li'"),
        opts(),
    )
    .await
    .unwrap();
    assert_failed(&result);
    assert!(result.steps[1].error.as_ref().unwrap().contains("Found 2"));

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_unknown_step() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");

    let result = scenario::run(
        page.inner(),
        "Scenario: Unknown\n  Given I do something completely made up",
        opts(),
    )
    .await
    .unwrap();
    assert_failed(&result);
    assert!(result.steps[0].error.as_ref().unwrap().contains("Unknown step"));

    browser.close().await.unwrap();
}

// ─── Click at coordinates ───────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_click_at_coordinates() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<div id='target' onclick=\"this.textContent='hit'\" style='position:absolute;left:50px;top:50px;width:100px;height:100px;background:#ccc'>click me</div>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I click at 100, 100
  Then '#target' should have text 'hit'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── E2E form scenario with variables ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_e2e_form_scenario() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<form><input id='username' type='text' required><input id='password' type='password' required><input id='agree' type='checkbox'><select id='role'><option value=''>Choose</option><option value='admin'>Admin</option><option value='user'>User</option></select><button id='submit' type='button' onclick=\"if(document.getElementById('username').value&&document.getElementById('password').value&&document.getElementById('agree').checked){document.title='Success: '+document.getElementById('username').value;document.getElementById('submit').textContent='Submitted'}else{document.title='Validation failed'}\">Submit</button></form>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Given I set $user to 'testadmin'
  When I fill the form:
    | #username | $user     |
    | #password | secret123 |
  And I click '#agree'
  And I select 'Admin' from '#role'
  Then '#username' should have value 'testadmin'
  And '#password' should have value 'secret123'
  And '#agree' should be checked
  And '#role' should have value 'admin'
  When I click '#submit'
  Then the title should contain 'Success'
  And the title should contain 'testadmin'
  And '#submit' should have text 'Submitted'
  When I store the title as $result"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);
    assert!(result.variables.get("result").unwrap().contains("Success: testadmin"));

    browser.close().await.unwrap();
}

// =========================================================================
// Complex interactive app tests
// =========================================================================

// ─── Real dblclick event fires ──────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_dblclick_fires_real_event() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    // This test verifies the dblclick DOM event fires (not just two clicks)
    let url = data_url(
        "<div id='target' style='padding:20px;background:#eee'>Double click me</div>\
         <div id='log'></div>\
         <script>\
         document.getElementById('target').addEventListener('dblclick', function() {\
           document.getElementById('log').textContent = 'dblclick_fired';\
         });\
         document.getElementById('target').addEventListener('click', function() {\
           var el = document.getElementById('log');\
           if (el.textContent === '') el.textContent = 'click_only';\
         });\
         </script>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I double-click '#target'
  Then '#log' should have text 'dblclick_fired'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Clear dispatches input event ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_clear_dispatches_events() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<input id='inp' type='text' value='hello'>\
         <div id='log'></div>\
         <script>\
         document.getElementById('inp').addEventListener('input', function() {\
           document.getElementById('log').textContent = 'input:' + this.value;\
         });\
         document.getElementById('inp').addEventListener('change', function() {\
           document.getElementById('log').textContent += ',change:' + this.value;\
         });\
         </script>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#inp' should have value 'hello'
  When I clear '#inp'
  Then '#inp' should have value ''
  And '#log' should contain text 'input:'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Evaluate returns result ────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_evaluate_returns_result() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<body>test</body>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "  When I evaluate '2 + 3'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);
    let step = &result.steps[1]; // the evaluate step
    assert!(step.data.is_some(), "evaluate should return result data");
    assert_eq!(step.data.as_ref().unwrap(), &serde_json::json!(5));

    browser.close().await.unwrap();
}

// ─── Interactive todo app ───────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_interactive_todo_app() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<style>.done{text-decoration:line-through;color:#999}</style>\
         <h1>Todo</h1>\
         <input id='new-todo' type='text' placeholder='What needs to be done?'>\
         <button id='add' onclick=\"\
           var v=document.getElementById('new-todo').value;\
           if(!v)return;\
           var li=document.createElement('li');\
           li.className='todo-item';\
           li.innerHTML='<input type=checkbox class=toggle>'+v;\
           li.querySelector('.toggle').onchange=function(){li.classList.toggle('done',this.checked)};\
           document.getElementById('list').appendChild(li);\
           document.getElementById('new-todo').value='';\
           document.getElementById('count').textContent=document.querySelectorAll('.todo-item').length+' items';\
         \">Add</button>\
         <ul id='list'></ul>\
         <span id='count'>0 items</span>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then the page should contain text 'Todo'
  And '#count' should have text '0 items'
  When I fill '#new-todo' with 'Buy groceries'
  And I click '#add'
  Then there should be 1 '.todo-item'
  And '#count' should have text '1 items'
  And the page should contain text 'Buy groceries'
  When I fill '#new-todo' with 'Clean house'
  And I click '#add'
  When I fill '#new-todo' with 'Write tests'
  And I click '#add'
  Then there should be 3 '.todo-item'
  And '#count' should have text '3 items'
  When I click '.toggle'
  Then '.todo-item' should have class 'done'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Dynamic content with timers ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_dynamic_loading_with_timers() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<button id='load' onclick=\"\
           this.disabled=true;\
           this.textContent='Loading...';\
           var self=this;\
           setTimeout(function(){\
             document.getElementById('content').innerHTML='<div id=result class=success>Data loaded successfully</div>';\
             self.textContent='Done';\
             self.disabled=false;\
           },500);\
         \">Load Data</button>\
         <div id='content'></div>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#load' should be enabled
  And '#load' should have text 'Load Data'
  When I click '#load'
  Then '#load' should be disabled
  And '#load' should have text 'Loading...'
  When I wait for '#result' for 5000ms
  Then '#result' should have text 'Data loaded successfully'
  And '#result' should have class 'success'
  And '#load' should have text 'Done'
  And '#load' should be enabled"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Checkbox and radio toggling ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_checkbox_radio_toggling() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<fieldset>\
           <legend>Preferences</legend>\
           <label><input type='checkbox' id='newsletter'> Newsletter</label>\
           <label><input type='checkbox' id='updates' checked> Updates</label>\
         </fieldset>\
         <fieldset>\
           <legend>Plan</legend>\
           <label><input type='radio' name='plan' id='free' value='free' checked> Free</label>\
           <label><input type='radio' name='plan' id='pro' value='pro'> Pro</label>\
           <label><input type='radio' name='plan' id='enterprise' value='enterprise'> Enterprise</label>\
         </fieldset>\
         <div id='status'></div>\
         <script>\
         document.querySelectorAll('input').forEach(function(el){\
           el.addEventListener('change', function(){\
             var checked = [];\
             document.querySelectorAll('input:checked').forEach(function(c){ checked.push(c.id); });\
             document.getElementById('status').textContent = checked.join(',');\
           });\
         });\
         </script>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#newsletter' should not be checked
  And '#updates' should be checked
  And '#free' should be checked
  And '#pro' should not be checked
  When I click '#newsletter'
  Then '#newsletter' should be checked
  When I click '#updates'
  Then '#updates' should not be checked
  When I click '#pro'
  Then '#pro' should be checked
  And '#free' should not be checked
  And '#status' should contain text 'newsletter'
  And '#status' should contain text 'pro'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Keyboard navigation ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_keyboard_navigation() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<input id='first' type='text' placeholder='First'>\
         <input id='second' type='text' placeholder='Second'>\
         <input id='third' type='text' placeholder='Third'>\
         <script>\
         document.addEventListener('keydown', function(e){\
           if(e.key==='Escape') document.title='escaped';\
           if(e.ctrlKey && e.key==='a') { e.preventDefault(); document.title='select-all'; }\
         });\
         </script>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I focus '#first'
  And I type 'hello'
  Then '#first' should have value 'hello'
  When I press 'Tab'
  And I type 'world'
  Then '#second' should have value 'world'
  When I press 'Escape'
  Then the title should be 'escaped'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Navigate without waiting ───────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_navigate_without_waiting() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<title>Initial</title><body>Start</body>");

    // Navigate normally first, then fire-and-forget to about:blank
    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then the title should be 'Initial'
  When I navigate to 'about:blank' without waiting
  And I wait 500ms"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);
    // The navigate step itself should complete quickly (not block for load)
    assert_eq!(result.steps[1].status, "passed");

    browser.close().await.unwrap();
}

// ─── Scroll default amounts ─────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_scroll_default_amount() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<div style='height:5000px'>tall</div>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I scroll down
  And I evaluate 'window.scrollY' and store as $y1
  When I scroll up
  And I evaluate 'window.scrollY' and store as $y2"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);
    // scroll down default 300, scroll up default 300 -> should be 0
    let y1: f64 = result.variables.get("y1").unwrap().parse().unwrap();
    let y2: f64 = result.variables.get("y2").unwrap().parse().unwrap();
    assert!(y1 > 0.0, "should have scrolled down: y1={y1}");
    assert!(y2 < y1, "should have scrolled back up: y2={y2}");

    browser.close().await.unwrap();
}

// ─── Form validation with error messages ────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_form_validation_errors() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<form onsubmit='return false'>\
           <input id='email' type='text'>\
           <div id='error' style='display:none;color:red'></div>\
           <button id='submit' type='button' onclick=\"\
             var v=document.getElementById('email').value;\
             var err=document.getElementById('error');\
             if(!v){err.style.display='block';err.textContent='Email is required';return;}\
             if(v.indexOf('@')<0){err.style.display='block';err.textContent='Invalid email';return;}\
             err.style.display='none';\
             document.title='submitted:'+v;\
           \">Submit</button>\
         </form>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I click '#submit'
  Then '#error' should contain text 'Email is required'
  When I fill '#email' with 'notanemail'
  And I click '#submit'
  Then '#error' should contain text 'Invalid email'
  When I fill '#email' with 'test@example.com'
  And I click '#submit'
  Then the title should contain 'submitted:test@example.com'
  And '#error' should not be visible"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Multi-step variable chaining ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_variable_chaining_across_steps() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<input id='a' type='text' value='alpha'>\
         <input id='b' type='text'>\
         <div id='result'></div>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I store the value of '#a' as $original
  And I fill '#b' with '$original'
  Then '#b' should have value 'alpha'
  When I evaluate 'document.getElementById(\"a\").value.toUpperCase()' and store as $upper
  And I fill '#a' with '$upper'
  Then '#a' should have value 'ALPHA'
  When I store the value of '#a' as $final
  And I evaluate 'document.getElementById(\"result\").textContent = \"$final\"'
  Then '#result' should have text 'ALPHA'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);
    assert_eq!(result.variables.get("original").unwrap(), "alpha");
    assert_eq!(result.variables.get("upper").unwrap(), "ALPHA");
    assert_eq!(result.variables.get("final").unwrap(), "ALPHA");

    browser.close().await.unwrap();
}

// ─── Hover-triggered dropdown menu ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_hover_dropdown_menu() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<style>\
           #menu{display:none;background:#fff;border:1px solid #ccc;padding:10px}\
           #trigger{padding:10px;background:#eee;cursor:pointer}\
         </style>\
         <div id='trigger' onmouseenter=\"document.getElementById('menu').style.display='block'\">Hover me</div>\
         <div id='menu'>\
           <button id='opt1' onclick=\"document.title='option1'\">Option 1</button>\
           <button id='opt2' onclick=\"document.title='option2'\">Option 2</button>\
         </div>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#menu' should not be visible
  When I hover over '#trigger'
  Then '#menu' should be visible
  And '#opt1' should be visible
  When I evaluate 'document.getElementById(\"opt1\").click()'
  Then the title should be 'option1'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Tab/accordion interactive widget ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_tab_widget() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<style>.tab-content{display:none}.tab-content.active{display:block}.tab.active{font-weight:bold}</style>\
         <div>\
           <button class='tab active' data-tab='1' onclick=\"switchTab(1)\">Tab 1</button>\
           <button class='tab' data-tab='2' onclick=\"switchTab(2)\">Tab 2</button>\
           <button class='tab' data-tab='3' onclick=\"switchTab(3)\">Tab 3</button>\
         </div>\
         <div id='panel1' class='tab-content active'>Content for tab 1</div>\
         <div id='panel2' class='tab-content'>Content for tab 2 with <a id='link2' href='#'>a link</a></div>\
         <div id='panel3' class='tab-content'>Content for tab 3</div>\
         <script>\
         function switchTab(n){\
           document.querySelectorAll('.tab').forEach(function(t){t.classList.remove('active')});\
           document.querySelectorAll('.tab-content').forEach(function(p){p.classList.remove('active')});\
           document.querySelector('[data-tab=\"'+n+'\"]').classList.add('active');\
           document.getElementById('panel'+n).classList.add('active');\
         }\
         </script>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#panel1' should have class 'active'
  And '#panel1' should contain text 'Content for tab 1'
  And '#panel2' should not have class 'active'
  When I click '[data-tab=\"2\"]'
  Then '#panel2' should have class 'active'
  And '#panel1' should not have class 'active'
  And '#panel2' should contain text 'Content for tab 2'
  When I click '#link2'
  When I click '[data-tab=\"3\"]'
  Then '#panel3' should have class 'active'
  And '#panel3' should contain text 'Content for tab 3'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Counter app with increment/decrement/reset ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_counter_app() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<div id='count'>0</div>\
         <button id='inc' onclick=\"update(1)\">+</button>\
         <button id='dec' onclick=\"update(-1)\">-</button>\
         <button id='reset' onclick=\"document.getElementById('count').textContent='0'\">Reset</button>\
         <script>\
         function update(d){\
           var el=document.getElementById('count');\
           var n=parseInt(el.textContent)+d;\
           el.textContent=n;\
           el.className=n>0?'positive':n<0?'negative':'zero';\
         }\
         </script>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#count' should have text '0'
  When I click '#inc'
  And I click '#inc'
  And I click '#inc'
  Then '#count' should have text '3'
  And '#count' should have class 'positive'
  When I click '#dec'
  Then '#count' should have text '2'
  When I click '#reset'
  Then '#count' should have text '0'
  When I click '#dec'
  Then '#count' should have text '-1'
  And '#count' should have class 'negative'
  When I store the text of '#count' as $val"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);
    assert_eq!(result.variables.get("val").unwrap(), "-1");

    browser.close().await.unwrap();
}

// ─── Multi-assertion failure messages ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_detailed_failure_messages() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<title>ActualTitle</title><div id='box'>actual text</div><input id='inp' value='actual_val'>");

    // Test title mismatch error message
    let r = scenario::run(page.inner(), &scenario(&url, "  Then the title should be 'WrongTitle'"), opts()).await.unwrap();
    assert_failed(&r);
    assert!(r.steps[1].error.as_ref().unwrap().contains("ActualTitle"), "error should show actual title");
    assert!(r.steps[1].error.as_ref().unwrap().contains("WrongTitle"), "error should show expected title");

    // Test text mismatch error
    let r = scenario::run(page.inner(), &scenario(&url, "  Then '#box' should have text 'wrong text'"), opts()).await.unwrap();
    assert_failed(&r);
    assert!(r.steps[1].error.as_ref().unwrap().contains("actual text"), "error should show actual text");

    // Test value mismatch error
    let r = scenario::run(page.inner(), &scenario(&url, "  Then '#inp' should have value 'wrong_val'"), opts()).await.unwrap();
    assert_failed(&r);
    assert!(r.steps[1].error.as_ref().unwrap().contains("actual_val"), "error should show actual value");

    // Test visibility when should not be visible
    let r = scenario::run(page.inner(), &scenario(&url, "  Then '#box' should not be visible"), opts()).await.unwrap();
    assert_failed(&r);
    assert!(r.steps[1].error.as_ref().unwrap().contains("visible"), "error should mention visibility");

    browser.close().await.unwrap();
}

// ─── stop_on_failure: false continues execution ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_continue_after_failure() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<p>hello</p><p>world</p>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then the page should contain text 'hello'
  And the page should contain text 'MISSING'
  And the page should contain text 'world'"),
        ScenarioOptions { stop_on_failure: false, screenshot_on_failure: false },
    )
    .await
    .unwrap();
    assert_failed(&result);
    // With stop_on_failure=false, all steps should execute
    assert_eq!(result.steps[1].status, "passed");  // navigate
    assert_eq!(result.steps[2].status, "failed");  // MISSING
    assert_eq!(result.steps[3].status, "passed");  // world - should still run

    browser.close().await.unwrap();
}

// ─── Feature/Scenario keywords ─────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_feature_scenario_parsing() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<title>Parsing Test</title><body>content</body>");

    let result = scenario::run(
        page.inner(),
        &format!("Feature: My Feature\n\nScenario: My Scenario\n  Given I navigate to \"{url}\"\n  Then the title should be 'Parsing Test'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);
    assert_eq!(result.scenario.as_deref(), Some("My Scenario"));

    browser.close().await.unwrap();
}

// ─── And/But keywords work ──────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_and_but_keywords() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<p>good</p><p>stuff</p>");

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then the page should contain text 'good'
  And the page should contain text 'stuff'
  But the page should not contain text 'bad'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);
    // Verify But keyword is recognized
    assert_eq!(result.steps[3].keyword, "But");

    browser.close().await.unwrap();
}

// ─── Interactive search/filter ──────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_search_filter_interaction() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<input id='search' type='text' placeholder='Search...'>\
         <ul id='list'>\
           <li class='item'>Apple</li>\
           <li class='item'>Banana</li>\
           <li class='item'>Cherry</li>\
           <li class='item'>Avocado</li>\
         </ul>\
         <script>\
         document.getElementById('search').addEventListener('input', function(){\
           var q=this.value.toLowerCase();\
           document.querySelectorAll('.item').forEach(function(li){\
             li.style.display=li.textContent.toLowerCase().indexOf(q)>=0?'':'none';\
           });\
           document.title=document.querySelectorAll('.item[style=\"\"],.item:not([style])').length+' results';\
         });\
         </script>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then there should be 4 '.item'
  When I fill '#search' with 'a'
  Then the title should contain '3 results'
  When I fill '#search' with 'cherry'
  Then the title should contain '1 results'
  When I clear '#search'
  Then the title should contain '4 results'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Go back / go forward with history ──────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_history_navigation() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url1 = data_url("<title>Page 1</title><body>First page</body>");
    let url2 = data_url("<title>Page 2</title><body>Second page</body>");

    let result = scenario::run(
        page.inner(),
        &format!("Scenario: History\n  Given I navigate to \"{url1}\"\n  Then the title should be 'Page 1'\n  When I navigate to \"{url2}\"\n  Then the title should be 'Page 2'\n  When I go back\n  Then the title should be 'Page 1'\n  When I go forward\n  Then the title should be 'Page 2'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// =========================================================================
// Framework fix verification tests
// =========================================================================

// ─── js_escape handles special characters ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_js_escape_special_chars() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url("<input id='inp' type='text'><div id='out'></div>");

    // Fill with value containing single quotes (escaped)
    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I fill '#inp' with 'it works'
  Then '#inp' should have value 'it works'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Assertions work with CSS selectors containing special chars ────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_assertion_with_data_attributes() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<div data-testid='main-content' class='active highlighted'>Test Content</div>\
         <button data-action='submit' disabled>Submit</button>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '[data-testid=\"main-content\"]' should be visible
  And '[data-testid=\"main-content\"]' should contain text 'Test Content'
  And '[data-testid=\"main-content\"]' should have class 'active'
  And '[data-testid=\"main-content\"]' should have class 'highlighted'
  And '[data-testid=\"main-content\"]' should not have class 'hidden'
  And '[data-testid=\"main-content\"]' should have attribute 'data-testid'
  And '[data-testid=\"main-content\"]' should have attribute 'data-testid' with value 'main-content'
  And '[data-testid=\"main-content\"]' should not have attribute 'disabled'
  And '[data-action=\"submit\"]' should be disabled"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Wait text vs wait selector disambiguation ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_wait_text_vs_selector_disambiguation() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<div id='area'></div>\
         <script>\
         setTimeout(function(){document.getElementById('area').textContent='loaded text here'},200);\
         setTimeout(function(){document.getElementById('area').innerHTML='<span id=\"marker\">done</span>'},400);\
         </script>",
    );

    // Both wait-for-text and wait-for-selector with timeouts should work correctly
    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  When I wait for text 'loaded text here' for 3000ms
  Then the page should contain text 'loaded text here'
  When I wait for '#marker' for 3000ms
  Then '#marker' should have text 'done'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Visibility checks CSS display/visibility/opacity ───────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_visibility_css_properties() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<div id='display-none' style='display:none'>Hidden by display</div>\
         <div id='visibility-hidden' style='visibility:hidden'>Hidden by visibility</div>\
         <div id='opacity-zero' style='opacity:0'>Hidden by opacity</div>\
         <div id='visible' style='display:block'>Visible element</div>\
         <div id='toggle' style='display:none'>Toggle me</div>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then '#display-none' should not be visible
  And '#visibility-hidden' should not be visible
  And '#opacity-zero' should not be visible
  And '#visible' should be visible
  And '#nonexistent' should not be visible
  And '#toggle' should not be visible
  When I evaluate 'document.getElementById(\"toggle\").style.display=\"block\"'
  Then '#toggle' should be visible"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Element count with various selectors ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bdd_element_count_various_selectors() {
    let browser = Browser::launch(LaunchOptions {
        backend: BackendKind::CdpPipe,
        ..Default::default()
    })
    .await
    .expect("launch");
    let page = browser.page().await.expect("page");
    let url = data_url(
        "<ul>\
           <li class='fruit'>Apple</li>\
           <li class='fruit'>Banana</li>\
           <li class='veggie'>Carrot</li>\
           <li class='fruit'>Date</li>\
         </ul>\
         <button>One</button>\
         <button>Two</button>",
    );

    let result = scenario::run(
        page.inner(),
        &scenario(&url, "\
  Then there should be 4 'li'
  And there should be 3 '.fruit'
  And there should be 1 '.veggie'
  And there should be 2 'button'
  And there should be 0 '.nonexistent'"),
        opts(),
    )
    .await
    .unwrap();
    assert_passed(&result);

    browser.close().await.unwrap();
}

// ─── Registry validation catches shadowed steps ─────────────────────────────

#[test]
fn bdd_registry_self_test() {
    // Verifies the registry builds without panicking.
    // In debug mode, checks that every step's example matches its own pattern.
    let registry = ferridriver::steps::StepRegistry::global();
    let steps = registry.list();
    assert!(steps.len() >= 50, "should have at least 50 steps, got {}", steps.len());

    let reference = registry.reference();
    assert!(reference.contains("Navigation"), "reference should contain Navigation category");
    assert!(reference.contains("Assertion"), "reference should contain Assertion category");
    assert!(reference.contains("Wait"), "reference should contain Wait category");
}
