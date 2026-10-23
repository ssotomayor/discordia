return (async () => {
  window.__dxfStreamEscapeOff?.();
  const onKey = event => {
    if (event.key !== 'Escape' || !document.querySelector('.dxf-streams-expanded')) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    dioxus.send(null);
  };
  window.addEventListener('keydown', onKey, { capture: true });
  let stop;
  const stopped = new Promise(resolve => { stop = resolve; });
  const off = () => {
    window.removeEventListener('keydown', onKey, { capture: true });
    stop();
  };
  window.__dxfStreamEscapeOff = off;
  try {
    await stopped;
  } finally {
    off();
    if (window.__dxfStreamEscapeOff === off) delete window.__dxfStreamEscapeOff;
  }
})();
