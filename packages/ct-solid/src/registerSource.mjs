/**
 * @ferridriver/ct-solid — registerSource
 *
 * Implements window.__ferriMount/Update/Unmount using Solid's render API.
 */

import { render } from "solid-js/web";

const __disposers = new Map();

window.__ferriMount = async function (componentRef, rootElement, options = {}) {
  const props = options.props || componentRef.props || {};

  let Component;
  if (window.__ferriRegistry && window.__ferriRegistry.resolveImportRef) {
    Component = await window.__ferriRegistry.resolveImportRef(componentRef);
  } else {
    throw new Error("Component registry not initialized");
  }

  if (window.__ferriBeforeMount) {
    for (const hook of window.__ferriBeforeMount) {
      await hook({ Component, props, hooksConfig: options.hooksConfig });
    }
  }

  const dispose = render(() => Component(props), rootElement);
  __disposers.set(rootElement, dispose);

  if (window.__ferriAfterMount) {
    for (const hook of window.__ferriAfterMount) {
      await hook({ Component, props, rootElement, hooksConfig: options.hooksConfig });
    }
  }
};

window.__ferriUnmount = function (rootElement) {
  const el = rootElement || document.getElementById("root") || document.getElementById("app");
  const dispose = __disposers.get(el);
  if (dispose) {
    dispose();
    __disposers.delete(el);
  }
};

window.__ferriBeforeMount = window.__ferriBeforeMount || [];
window.__ferriAfterMount = window.__ferriAfterMount || [];
