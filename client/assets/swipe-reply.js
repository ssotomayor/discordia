return (function () {
  if (window.__dxfSwipeReplyOff) window.__dxfSwipeReplyOff();
  // Pixels of leftward pull: where the gesture is decided, where it arms, where it stops following.
  const SLOP = 10, ARM = 56, LIMIT = 84, SETTLE_MS = 200;
  let row = null, id = null, x0 = 0, y0 = 0, decided = false, tracking = false, armed = false;
  const listeners = [];
  const on = (name, fn) => {
    document.addEventListener(name, fn, true);
    listeners.push(() => document.removeEventListener(name, fn, true));
  };
  const settle = (el) => {
    el.classList.remove('dxf-swiping', 'dxf-swipe-armed');
    el.classList.add('dxf-swipe-settling');
    el.style.setProperty('--swipe', '0px');
    el.style.setProperty('--swipe-progress', '0');
    setTimeout(() => {
      el.classList.remove('dxf-swipe-settling');
      el.style.removeProperty('--swipe');
      el.style.removeProperty('--swipe-progress');
    }, SETTLE_MS);
  };
  const reset = () => {
    if (row && tracking) settle(row);
    row = null; id = null; decided = false; tracking = false; armed = false;
  };
  const reply = (el) => {
    // Focus first, inside the gesture: a later focus() would not raise the keyboard.
    const field = document.querySelector('[id^="dxf-composer-"][contenteditable="true"]');
    const api = field && window.__dxfComposers && window.__dxfComposers[field.id];
    if (api) api.focus(); else if (field) field.focus();
    const button = el.querySelector('[data-context-action="reply"]');
    if (button && !button.disabled) button.click();
  };
  on('pointerdown', (e) => {
    if (e.pointerType !== 'touch' || !e.isPrimary || !e.target.closest) return;
    const target = e.target.closest('[data-context-message]');
    if (!target || !target.querySelector('[data-context-action="reply"]')) return;
    if (e.target.closest('button, input, textarea, [contenteditable="true"], video, audio')) return;
    row = target; id = e.pointerId; x0 = e.clientX; y0 = e.clientY;
    decided = false; tracking = false; armed = false;
  });
  on('pointermove', (e) => {
    if (!row || e.pointerId !== id) return;
    const dx = e.clientX - x0, dy = e.clientY - y0;
    if (!decided) {
      if (Math.abs(dx) < SLOP && Math.abs(dy) < SLOP) return;
      decided = true;
      // Vertical is the list's scroll and rightward is the pager's page turn.
      if (dx > -SLOP || Math.abs(dy) > Math.abs(dx)) { row = null; return; }
      tracking = true;
      row.classList.add('dxf-swiping');
      try { row.setPointerCapture(id); } catch (_) {}
    }
    if (!tracking) return;
    const pull = Math.min(LIMIT, Math.max(0, -dx));
    const shown = pull <= ARM ? pull : ARM + (pull - ARM) * 0.35;
    row.style.setProperty('--swipe', -shown + 'px');
    row.style.setProperty('--swipe-progress', String(Math.min(1, pull / ARM)));
    const now = pull >= ARM;
    if (now !== armed) {
      armed = now;
      row.classList.toggle('dxf-swipe-armed', armed);
      if (armed && navigator.vibrate) { try { navigator.vibrate(8); } catch (_) {} }
    }
  });
  on('pointerup', (e) => {
    if (!row || e.pointerId !== id) return;
    const target = row, fire = tracking && armed;
    reset();
    if (fire) reply(target);
  });
  on('pointercancel', (e) => { if (row && e.pointerId === id) reset(); });
  const off = () => { listeners.forEach(remove => remove()); reset(); delete window.__dxfSwipeReplyOff; };
  window.__dxfSwipeReplyOff = off;
})();
