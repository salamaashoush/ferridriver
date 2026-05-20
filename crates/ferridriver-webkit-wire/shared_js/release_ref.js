// Body for OP_RELEASE_REF. Drops `window.__wr.<ref_id>` so the
// JSHandle's page-side root goes away.
if (window.__wr && typeof window.__wr.delete === 'function') {
  window.__wr.delete(__fd_ref_id);
}
return null;
