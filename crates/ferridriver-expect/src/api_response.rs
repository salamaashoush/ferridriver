//! `expect(apiResponse).toBeOK()` — synchronous status check on a
//! captured `ferridriver::http_client::HttpResponse`.

use ferridriver::http_client::HttpResponse;

use crate::AssertionFailure;
use crate::builder::Expect;

impl Expect<'_, HttpResponse> {
  pub fn to_be_ok(&self) -> Result<(), AssertionFailure> {
    let resp = self.subject;
    let status = resp.status();
    let pass_raw = (200..300).contains(&status);
    let pass = if self.is_not { !pass_raw } else { pass_raw };
    if pass {
      return Ok(());
    }
    let url = resp.url();
    let status_text = resp.status_text();
    let not = if self.is_not { ".not" } else { "" };
    Err(AssertionFailure::new(
      format!("expect(response){not}.toBeOK() failed"),
      Some(format!(
        "URL:      {url}\nStatus:   {status} {status_text}"
      )),
    ))
  }
}
