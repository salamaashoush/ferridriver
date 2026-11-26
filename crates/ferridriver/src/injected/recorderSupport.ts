import { Highlight } from './highlight';
import { generateSelector } from './selectorGenerator';
import { asLocator } from '@isomorphic/locatorGenerators';

declare global {
  interface Window {
    __fdDevtools?: {
      Highlight: typeof Highlight;
      generateSelector: typeof generateSelector;
      asLocator: typeof asLocator;
    };
  }
}

(() => {
  if (window.__fdDevtools instanceof Object)
    return;
  window.__fdDevtools = {
    Highlight,
    generateSelector,
    asLocator,
  };
})();
