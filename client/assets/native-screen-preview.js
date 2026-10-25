function (id, revision) {
  window.__dxfNativeScreenPreview?.stop();
  let element = null, resize = null, pending = null, previous = '', stopped = false;
  function send(value) {
    const payload = { revision, ...value };
    const key = JSON.stringify(payload);
    if (key === previous) return;
    previous = key;
    try { dioxus.send(payload); } catch (_) {}
  }
  function report() {
    pending = null;
    if (stopped) return;
    if (!element) {
      element = document.getElementById(id);
      if (!element) return;
      resize = new ResizeObserver(schedule);
      resize.observe(element);
    }
    if (!element.isConnected) { stop(); return; }
    const r = element.getBoundingClientRect();
    const vw = window.innerWidth, vh = window.innerHeight;
    const style = getComputedStyle(element);
    let visible = !document.hidden && r.width > 0 && r.height > 0 && r.left >= -0.5 && r.top >= -0.5 &&
      r.right <= vw + 0.5 && r.bottom <= vh + 0.5 && style.display !== 'none' && style.visibility !== 'hidden';
    if (visible) {
      visible = [[0.5, 0.5], [0.1, 0.1], [0.9, 0.1], [0.1, 0.9], [0.9, 0.9]].every(([x, y]) =>
        element.contains(document.elementFromPoint(r.left + r.width * x, r.top + r.height * y)));
    }
    const round = value => Math.round(value * 100) / 100;
    send(visible ? { visible, left: round(r.left), top: round(r.top), width: round(r.width), height: round(r.height), viewportWidth: vw, viewportHeight: vh } : { visible: false });
  }
  function schedule() {
    if (!stopped && pending === null) pending = requestAnimationFrame(report);
  }
  const mutations = new MutationObserver(records => {
    if (!element || !element.isConnected || records.some(record => record.type === 'childList' ||
      record.target === element || record.target.contains(element) || record.target.closest('[role="dialog"], [aria-modal="true"], dialog'))) schedule();
  });
  mutations.observe(document.documentElement, { childList: true, subtree: true, attributes: true, attributeFilter: ['style', 'class', 'hidden', 'open'] });
  document.addEventListener('scroll', schedule, true);
  document.addEventListener('visibilitychange', schedule);
  window.addEventListener('resize', schedule);
  window.visualViewport?.addEventListener('resize', schedule);
  window.visualViewport?.addEventListener('scroll', schedule);
  function stop() {
    if (stopped) return;
    stopped = true;
    if (pending !== null) cancelAnimationFrame(pending);
    mutations.disconnect();
    resize?.disconnect();
    document.removeEventListener('scroll', schedule, true);
    document.removeEventListener('visibilitychange', schedule);
    window.removeEventListener('resize', schedule);
    window.visualViewport?.removeEventListener('resize', schedule);
    window.visualViewport?.removeEventListener('scroll', schedule);
    send({ visible: false });
    if (window.__dxfNativeScreenPreview?.revision === revision) delete window.__dxfNativeScreenPreview;
  }
  window.__dxfNativeScreenPreview = { revision, stop };
  report();
}
