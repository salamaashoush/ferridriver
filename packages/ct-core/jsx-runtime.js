/**
 * ferridriver CT JSX runtime.
 *
 * When test files use JSX (<Counter initial={5} />), the transpiler
 * (configured with importSource pointing here) calls jsx() instead of
 * React.createElement(). This produces lightweight descriptor objects
 * that can be serialized and sent to the browser.
 *
 * The browser-side unwrapObject() + framework registerSource converts
 * these descriptors back into real framework elements.
 */

function jsx(type, props, key) {
  return {
    __pw_type: "jsx",
    type, // importRef object OR Fragment symbol
    props: props || {},
    key: key !== undefined ? key : undefined,
  };
}

// jsxs is used for static children (multiple children optimization).
// We treat it the same as jsx.
const jsxs = jsx;

const Fragment = { __ferri_jsx_fragment: true };

module.exports = { Fragment, jsx, jsxs };
