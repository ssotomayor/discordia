function (instance) {
  window.__dxfImageViewerOff?.();
  const root = document.getElementById('dxf-image-viewer');
  if (!root) return;
  const stage = root.querySelector('[data-viewer-stage]');
  const image = root.querySelector('img');
  const label = root.querySelector('[data-viewer-zoom]');
  const previousFocus = document.activeElement;
  const pointers = new Map();
  const listeners = [];
  let scale = 1, x = 0, y = 0, frame = null, closed = false;
  let backgroundTap = false, moved = false, lastTap = null, touchZoomAt = -Infinity;
  const listen = (node, type, handler, options) => {
    node.addEventListener(type, handler, options);
    listeners.push(() => node.removeEventListener(type, handler, options));
  };
  const close = () => { if (!closed) { closed = true; dioxus.send('close'); } };
  const clamp = () => {
    scale = Math.max(1, Math.min(8, scale));
    const padding = getComputedStyle(stage);
    const width = stage.clientWidth - parseFloat(padding.paddingLeft) - parseFloat(padding.paddingRight);
    const height = stage.clientHeight - parseFloat(padding.paddingTop) - parseFloat(padding.paddingBottom);
    const maxX = Math.max(0, (image.clientWidth * scale - width) / 2);
    const maxY = Math.max(0, (image.clientHeight * scale - height) / 2);
    x = Math.max(-maxX, Math.min(maxX, x));
    y = Math.max(-maxY, Math.min(maxY, y));
  };
  const paint = () => {
    frame = null;
    clamp();
    image.style.transform = `translate3d(${x}px, ${y}px, 0) scale(${scale})`;
    stage.style.cursor = scale > 1 ? (pointers.size ? 'grabbing' : 'grab') : 'zoom-in';
    label.textContent = `${Math.round(scale * 100)}%`;
    root.dataset.zoom = String(scale);
    root.querySelector('[data-viewer-action="out"]').disabled = scale <= 1;
    root.querySelector('[data-viewer-action="in"]').disabled = scale >= 8;
  };
  const draw = () => { if (frame === null) frame = requestAnimationFrame(paint); };
  const point = e => {
    const r = stage.getBoundingClientRect();
    return { x: e.clientX - r.left - image.offsetLeft - image.clientWidth / 2,
      y: e.clientY - r.top - image.offsetTop - image.clientHeight / 2 };
  };
  const zoom = (next, at = { x: 0, y: 0 }) => {
    next = Math.max(1, Math.min(8, next));
    const ratio = next / scale;
    x = at.x - (at.x - x) * ratio;
    y = at.y - (at.y - y) * ratio;
    scale = next;
  };
  const reset = () => { scale = 1; x = y = 0; draw(); };
  const pair = () => {
    const [a, b] = [...pointers.values()];
    return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2, distance: Math.hypot(a.x - b.x, a.y - b.y) };
  };
  listen(stage, 'wheel', e => {
    e.preventDefault();
    const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? stage.clientHeight : 1;
    zoom(scale * Math.exp(-Math.max(-300, Math.min(300, e.deltaY * unit)) * 0.003), point(e));
    draw();
  }, { passive: false });
  listen(stage, 'pointerdown', e => {
    if (e.button !== 0) return;
    const p = point(e);
    if (!pointers.size) { moved = false; backgroundTap = e.target !== image; }
    pointers.set(e.pointerId, { ...p, startX: p.x, startY: p.y });
    if (pointers.size > 1) { moved = true; backgroundTap = false; lastTap = null; }
    try { stage.setPointerCapture(e.pointerId); } catch (_) {}
    draw();
  });
  listen(stage, 'pointermove', e => {
    const old = pointers.get(e.pointerId);
    if (!old) return;
    const before = pointers.size === 2 ? pair() : null;
    const p = point(e);
    moved ||= Math.hypot(p.x - old.startX, p.y - old.startY) > 5;
    pointers.set(e.pointerId, { ...old, ...p });
    if (before) {
      const after = pair();
      zoom(scale * after.distance / Math.max(1, before.distance), before);
      x += after.x - before.x; y += after.y - before.y;
    } else if (pointers.size === 1 && scale > 1) {
      x += p.x - old.x; y += p.y - old.y;
    }
    draw();
  });
  const release = (e, cancelled) => {
    if (!pointers.has(e.pointerId)) return;
    const p = point(e);
    pointers.delete(e.pointerId);
    try { if (stage.hasPointerCapture(e.pointerId)) stage.releasePointerCapture(e.pointerId); } catch (_) {}
    if (!cancelled && !pointers.size && !moved) {
      if (backgroundTap) close();
      else if (e.pointerType === 'touch') {
        const now = performance.now();
        if (lastTap && now - lastTap.time < 300 && Math.hypot(p.x - lastTap.x, p.y - lastTap.y) < 25) {
          zoom(scale > 1 ? 1 : 2, p); touchZoomAt = now; lastTap = null;
        } else lastTap = { ...p, time: now };
      }
    }
    if (cancelled) { moved = true; lastTap = null; }
    draw();
  };
  listen(stage, 'pointerup', e => release(e, false));
  listen(stage, 'pointercancel', e => release(e, true));
  listen(stage, 'lostpointercapture', e => release(e, true));
  listen(stage, 'dblclick', e => {
    if ((e.target !== image && backgroundTap) || performance.now() - touchZoomAt < 400) return;
    e.preventDefault(); zoom(scale > 1 ? 1 : 2, point(e)); draw();
  });
  const control = e => e.target.closest('button[data-viewer-action]');
  const activate = button => {
    if (!button || button.disabled) return;
    const action = button.dataset.viewerAction;
    if (action === 'close') close();
    else if (action === 'reset') reset();
    else { zoom(scale * (action === 'in' ? 1.25 : 0.8)); draw(); }
  };
  let controlTap = null, handledTap = null;
  listen(root, 'pointerdown', e => {
    handledTap = null;
    controlTap = e.pointerType === 'touch' && control(e)
      ? { button: control(e), id: e.pointerId, x: e.clientX, y: e.clientY } : null;
  });
  listen(root, 'pointermove', e => {
    if (controlTap?.id === e.pointerId && Math.hypot(e.clientX - controlTap.x, e.clientY - controlTap.y) > 10) controlTap = null;
  });
  listen(root, 'pointercancel', () => { controlTap = null; });
  listen(root, 'pointerup', e => {
    if (controlTap?.id === e.pointerId && controlTap.button === control(e)) {
      activate(controlTap.button);
      handledTap = { button: controlTap.button, time: performance.now() };
    }
    controlTap = null;
  });
  listen(root, 'click', e => {
    const button = control(e);
    if (!button) return;
    e.preventDefault();
    if (e.detail && handledTap?.button === button && performance.now() - handledTap.time < 500) return;
    activate(button);
  });
  listen(document, 'keydown', e => {
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); return; }
    if (e.key === 'Tab') {
      e.preventDefault(); e.stopPropagation();
      const buttons = [...root.querySelectorAll('button:not(:disabled)')];
      const index = buttons.indexOf(document.activeElement);
      buttons[index < 0 ? (e.shiftKey ? buttons.length - 1 : 0) : (index + (e.shiftKey ? -1 : 1) + buttons.length) % buttons.length]?.focus();
      return;
    }
    if (['+', '=', '-', '_', '0', 'ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(e.key)) {
      e.preventDefault(); e.stopPropagation();
      if (e.key === '0') reset();
      else if (e.key === '+' || e.key === '=') zoom(scale * 1.25);
      else if (e.key === '-' || e.key === '_') zoom(scale * 0.8);
      else if (scale > 1) {
        x += e.key === 'ArrowLeft' ? 40 : e.key === 'ArrowRight' ? -40 : 0;
        y += e.key === 'ArrowUp' ? 40 : e.key === 'ArrowDown' ? -40 : 0;
      }
      draw();
    }
  }, true);
  listen(image, 'load', draw);
  listen(image, 'dragstart', e => e.preventDefault());
  listen(window, 'resize', draw);
  const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(draw);
  observer?.observe(stage);
  const off = () => {
    listeners.splice(0).forEach(remove => remove());
    observer?.disconnect();
    pointers.clear();
    if (frame !== null) cancelAnimationFrame(frame);
    if (window.__dxfImageViewerOff === off) {
      delete window.__dxfImageViewerOff;
      if (previousFocus?.isConnected && root.contains(document.activeElement)) previousFocus.focus({ preventScroll: true });
    }
  };
  off.instance = instance;
  window.__dxfImageViewerOff = off;
  root.focus({ preventScroll: true });
  paint();
}
