(() => {
  if (window.dxStreamControls) return;
  const selector = '.dxf-stream-tile[data-stream-controls]';
  const controls = '.dxf-stream-toolbar, .dxf-stream-footer';
  const states = new Map();
  const idleMs = 2500;
  let keyboard = false;

  function install(tile) {
    if (states.has(tile)) return;
    let timer = 0;
    let inside = false;
    let pressed = false;
    let touchSurface = false;
    let touchAt = 0;
    let visible = true;
    const pinned = () => pressed || (keyboard && tile.contains(document.activeElement));
    const setVisible = value => {
      visible = value;
      tile.dataset.controlsVisible = String(value);
    };
    const clear = () => { clearTimeout(timer); timer = 0; };
    const hide = () => { clear(); if (!pinned()) setVisible(false); };
    const arm = () => {
      clear();
      if (!pinned()) timer = setTimeout(hide, idleMs);
    };
    const reveal = () => { setVisible(true); arm(); };
    const enter = event => { if (event.pointerType !== 'touch') { inside = true; reveal(); } };
    const move = event => {
      if (event.pointerType === 'touch') return;
      keyboard = false;
      inside = true;
      reveal();
    };
    const leave = () => { inside = false; hide(); };
    const down = event => {
      keyboard = false;
      const interactive = event.target.closest('button, input, select, textarea, a, [tabindex]');
      const onControl = interactive && interactive.closest(controls);
      touchSurface = event.pointerType === 'touch' && !onControl;
      touchAt = performance.now();
      if (touchSurface) {
        clear();
        if (visible) setVisible(false); else reveal();
      } else {
        pressed = !!onControl;
        reveal();
      }
    };
    const click = event => {
      if (touchSurface && performance.now() - touchAt < 1000) {
        touchSurface = false;
        event.preventDefault();
        event.stopImmediatePropagation();
      }
    };
    const focus = () => {
      if (!pressed && tile.querySelector(':focus-visible')) keyboard = true;
      reveal();
    };
    const blur = () => queueMicrotask(() => { if (inside) arm(); else hide(); });
    const bindings = [
      ['pointerenter', enter], ['pointermove', move], ['pointerleave', leave],
      ['pointerdown', down], ['click', click, true], ['focusin', focus], ['focusout', blur],
    ];
    bindings.forEach(([name, fn, capture]) => tile.addEventListener(name, fn, capture));
    states.set(tile, {
      reveal,
      release(event) {
        if (!pressed) return;
        pressed = false;
        const rect = tile.getBoundingClientRect();
        inside = event.type !== 'pointercancel' && event.clientX >= rect.left && event.clientX <= rect.right && event.clientY >= rect.top && event.clientY <= rect.bottom;
        if (inside) arm(); else hide();
      },
      cancel() { pressed = false; inside = false; hide(); },
      destroy() {
        clear();
        bindings.forEach(([name, fn, capture]) => tile.removeEventListener(name, fn, capture));
        tile.removeAttribute('data-controls-visible');
        states.delete(tile);
      },
    });
    reveal();
  }
  const scan = node => {
    if (!(node instanceof Element)) return;
    if (node.matches(selector)) install(node);
    node.querySelectorAll(selector).forEach(install);
  };
  const release = event => states.forEach(state => state.release(event));
  const cancel = () => { keyboard = false; states.forEach(state => state.cancel()); };
  const visibility = () => { if (document.hidden) cancel(); };
  const key = () => {
    keyboard = true;
    const tile = document.activeElement?.closest(selector);
    if (tile) states.get(tile)?.reveal();
  };
  const observer = new MutationObserver(records => {
    for (const record of records) {
      record.addedNodes.forEach(scan);
      const tile = record.target instanceof Element && record.target.closest(selector);
      if (tile) install(tile);
    }
    for (const [tile, state] of states) if (!tile.isConnected) state.destroy();
  });
  const destroy = () => {
    observer.disconnect();
    states.forEach(state => state.destroy());
    window.removeEventListener('pointerup', release, true);
    window.removeEventListener('pointercancel', release, true);
    document.removeEventListener('keydown', key, true);
    window.removeEventListener('blur', cancel);
    document.removeEventListener('visibilitychange', visibility);
    window.removeEventListener('pagehide', destroy);
    delete window.dxStreamControls;
  };
  document.querySelectorAll(selector).forEach(install);
  observer.observe(document.documentElement, { childList: true, subtree: true });
  window.addEventListener('pointerup', release, true);
  window.addEventListener('pointercancel', release, true);
  document.addEventListener('keydown', key, true);
  window.addEventListener('blur', cancel);
  document.addEventListener('visibilitychange', visibility);
  window.addEventListener('pagehide', destroy);
  window.dxStreamControls = { destroy };
})();
