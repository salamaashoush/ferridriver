import { Highlight } from './highlight';

(() => {
  const fd = (window as any).__fd;
  const injected = fd?._injected;
  if (!fd || !injected || fd.__recorderSupport)
    return;
  fd.__recorderSupport = true;

  let highlight: Highlight | undefined;
  injected.createHighlight = () => new Highlight(injected);
  injected.hideHighlight = () => {
    if (highlight) {
      highlight.uninstall();
      highlight = undefined;
    }
  };
  injected.addHighlight = (selector: any, style?: string) => {
    highlight ??= new Highlight(injected);
    highlight.install();
    highlight.addElementHighlight(selector, style);
  };
  injected.removeHighlight = (selector: any) => {
    highlight ??= new Highlight(injected);
    highlight.install();
    highlight.removeElementHighlight(selector);
  };
})();
