/**
 * @ferridriver/ct-react — registerSource
 *
 * Implements window.__ferriMount/Update/Unmount using React 18+ createRoot API.
 * This file is injected into the browser page by the CT framework.
 */

import React from "react";
import { createRoot } from "react-dom/client";

const __roots = new Map();

/**
 * Mount a React component into a DOM element.
 * @param {object} componentRef - { id, props, children } from the test
 * @param {HTMLElement} rootElement - DOM element to mount into
 * @param {object} options - { props, hooksConfig }
 */
window.__ferriMount = async function (componentRef, rootElement, options = {}) {
  const props = options.props || componentRef.props || {};

  // Resolve the component — either from registry or direct reference.
  let Component;
  if (window.__ferriRegistry && window.__ferriRegistry[componentRef.id]) {
    Component = await window.__ferriRegistry[componentRef.id]();
  } else {
    throw new Error(
      `Component "${componentRef.id}" not found in registry. ` +
        `Available: ${Object.keys(window.__ferriRegistry || {}).join(", ")}`
    );
  }

  // Run beforeMount hooks if any.
  if (window.__ferriBeforeMount) {
    for (const hook of window.__ferriBeforeMount) {
      await hook({ Component, props, hooksConfig: options.hooksConfig });
    }
  }

  const root = createRoot(rootElement);
  __roots.set(rootElement, { root, Component });

  // Render.
  root.render(React.createElement(Component, props));

  // Run afterMount hooks if any.
  if (window.__ferriAfterMount) {
    for (const hook of window.__ferriAfterMount) {
      await hook({ Component, props, rootElement, hooksConfig: options.hooksConfig });
    }
  }
};

/**
 * Update props on a mounted component (re-render).
 */
window.__ferriUpdate = function (rootElement, newProps) {
  const entry = __roots.get(rootElement);
  if (!entry) throw new Error("No component mounted on this element");
  entry.root.render(React.createElement(entry.Component, newProps));
};

/**
 * Unmount a component.
 */
window.__ferriUnmount = function (rootElement) {
  const el = rootElement || document.getElementById("root") || document.getElementById("app");
  const entry = __roots.get(el);
  if (entry) {
    entry.root.unmount();
    __roots.delete(el);
  }
};

// Hook arrays — users push into these in their playwright/index.ts equivalent.
window.__ferriBeforeMount = window.__ferriBeforeMount || [];
window.__ferriAfterMount = window.__ferriAfterMount || [];
