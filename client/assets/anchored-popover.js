function (id) {
  const panel = document.getElementById(id);
  if (!panel || !panel.parentElement) return;
  const anchor = panel.parentElement;
  // The top layer escapes the dialog's scrolling body, which would clip the panel.
  panel.showPopover();
  const place = () => {
    const rect = anchor.getBoundingClientRect();
    const width = panel.offsetWidth;
    const height = panel.offsetHeight;
    const left = Math.max(8, Math.min(rect.left, innerWidth - width - 8));
    const below = rect.bottom + 6;
    const top = below + height <= innerHeight - 8 ? below : Math.max(8, rect.top - height - 6);
    panel.style.left = `${left}px`;
    panel.style.top = `${top}px`;
  };
  place();
  // The chosen value starts in view; scrollIntoView would also scroll the dialog behind.
  for (const column of panel.querySelectorAll('.dxf-time-column')) {
    const chosen = column.querySelector('[aria-selected="true"]');
    if (chosen) column.scrollTop = chosen.offsetTop - column.offsetTop - (column.clientHeight - chosen.offsetHeight) / 2;
  }
  const events = new AbortController();
  window.addEventListener('resize', place, { signal: events.signal });
  window.addEventListener('scroll', place, { capture: true, signal: events.signal });
  const size = new ResizeObserver(place);
  size.observe(panel);
  const removal = new MutationObserver(() => {
    if (panel.isConnected) return;
    events.abort();
    size.disconnect();
    removal.disconnect();
  });
  removal.observe(document.body, { childList: true, subtree: true });
}
