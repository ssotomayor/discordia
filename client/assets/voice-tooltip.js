function (id) {
  const tip = document.getElementById(id);
  if (!tip || !tip.parentElement) return;
  const anchor = tip.parentElement;
  // The top layer escapes both the channel scroller's clipping and raised grid panels.
  tip.showPopover();
  const place = () => {
    const rect = anchor.getBoundingClientRect();
    const width = tip.offsetWidth;
    const height = tip.offsetHeight;
    const left = Math.max(8, Math.min(rect.right - width, innerWidth - width - 8));
    const above = rect.top - height - 6;
    const top = Math.max(8, Math.min(above >= 8 ? above : rect.bottom + 6, innerHeight - height - 8));
    tip.style.left = `${left}px`;
    tip.style.top = `${top}px`;
  };
  place();
  const events = new AbortController();
  window.addEventListener('resize', place, { signal: events.signal });
  window.addEventListener('scroll', place, { capture: true, signal: events.signal });
  const size = new ResizeObserver(place);
  size.observe(tip);
  const removal = new MutationObserver(() => {
    if (tip.isConnected) return;
    events.abort();
    size.disconnect();
    removal.disconnect();
  });
  removal.observe(document.body, { childList: true, subtree: true });
}
