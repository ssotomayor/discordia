(function (rootId) {
  const root = document.getElementById(rootId);
  if (!root) return;
  const owners = window.__dxfVoiceDrags ||= new Map();
  owners.get(rootId)?.();
  const controller = new AbortController();
  const signal = controller.signal;
  const send = dioxus.send.bind(dioxus);
  let disposed = false;
  let active = null, target = null, ghost = null, frame = 0, suppressClick = false, clickTimer = 0;
  const listen = (node, name, handler) => node.addEventListener(name, handler, { capture: true, passive: false, signal });
  function mark(next) {
    if (target === next) return;
    target?.removeAttribute('data-voice-drop-target');
    target = next;
    target?.setAttribute('data-voice-drop-target', '');
  }
  function hit() {
    if (!active) return;
    const next = document.elementFromPoint(active.x, active.y)?.closest('[data-voice-channel]');
    mark(next && root.contains(next) && next.dataset.voiceChannel !== active.from ? next : null);
  }
  function animate() {
    if (!active?.started) return;
    if (!active.row.isConnected) { cancel(); return; }
    const rect = root.getBoundingClientRect();
    if (active.x >= rect.left && active.x <= rect.right && active.y >= rect.top && active.y <= rect.bottom) {
      const edge = 28;
      const delta = active.y < rect.top + edge ? -8 : active.y > rect.bottom - edge ? 8 : 0;
      if (delta) root.scrollTop += delta;
    }
    hit();
    frame = requestAnimationFrame(animate);
  }
  function clearVisuals() {
    cancelAnimationFrame(frame);
    frame = 0;
    mark(null);
    ghost?.remove();
    ghost = null;
    root.removeAttribute('data-voice-dragging');
  }
  function cancel() {
    const previous = active;
    active = null;
    clearVisuals();
    if (previous && root.hasPointerCapture?.(previous.pointer)) root.releasePointerCapture(previous.pointer);
  }
  listen(root, 'pointerdown', e => {
    clearTimeout(clickTimer);
    suppressClick = false;
    if (e.button !== 0 || !e.isPrimary) return;
    const row = e.target.closest('[data-voice-pubkey]');
    if (!row || !root.contains(row)) return;
    e.stopPropagation();
    if (e.target.closest('button,input,select,a')) return;
    if (row.dataset.voiceMovable !== 'true') return;
    cancel();
    active = { row, pointer: e.pointerId, pubkey: row.dataset.voicePubkey, from: row.dataset.voiceFrom,
      startX: e.clientX, startY: e.clientY, x: e.clientX, y: e.clientY, started: false };
  });
  listen(root, 'dragstart', e => { if (active) e.preventDefault(); });
  listen(document, 'pointermove', e => {
    if (!active || e.pointerId !== active.pointer) return;
    if (e.pointerType === 'mouse' && !(e.buttons & 1)) { cancel(); return; }
    active.x = e.clientX;
    active.y = e.clientY;
    if (!active.started && Math.hypot(active.x - active.startX, active.y - active.startY) < 6) return;
    if (!active.started) {
      active.started = true;
      root.setPointerCapture?.(e.pointerId);
      root.setAttribute('data-voice-dragging', '');
      ghost = document.createElement('div');
      ghost.className = 'dxf-voice-drag-ghost';
      ghost.textContent = active.row.dataset.voiceName;
      ghost.setAttribute('popover', 'manual');
      document.body.appendChild(ghost);
      ghost.showPopover?.();
      frame = requestAnimationFrame(animate);
    }
    ghost.style.left = `${Math.min(active.x + 14, Math.max(8, innerWidth - ghost.offsetWidth - 8))}px`;
    ghost.style.top = `${Math.min(active.y + 14, Math.max(8, innerHeight - ghost.offsetHeight - 8))}px`;
    hit();
    e.preventDefault();
    e.stopPropagation();
  });
  listen(document, 'pointerup', e => {
    if (!active || e.pointerId !== active.pointer) return;
    const previous = active;
    active.x = e.clientX;
    active.y = e.clientY;
    hit();
    const destination = target?.dataset.voiceChannel;
    if (previous.started) {
      e.preventDefault();
      e.stopPropagation();
      suppressClick = true;
      clickTimer = setTimeout(() => { suppressClick = false; }, 0);
    }
    cancel();
    if (previous.started && previous.row.isConnected && destination) {
      send({ pubkey: previous.pubkey, from_channel_id: previous.from, channel_id: destination });
    }
  });
  listen(document, 'click', e => {
    if (suppressClick) { e.preventDefault(); e.stopImmediatePropagation(); suppressClick = false; }
  });
  listen(document, 'pointercancel', cancel);
  listen(document, 'keydown', e => { if (e.key === 'Escape' && active) { e.preventDefault(); cancel(); } });
  listen(window, 'blur', cancel);
  const observer = new MutationObserver(() => { if (!root.isConnected) dispose(); });
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    cancel();
    clearTimeout(clickTimer);
    controller.abort();
    observer.disconnect();
    if (owners.get(rootId) === dispose) owners.delete(rootId);
    send(null);
  };
  owners.set(rootId, dispose);
  observer.observe(root.parentNode, { childList: true });
})
